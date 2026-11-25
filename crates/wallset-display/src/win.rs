//! Windows display probing.
//!
//! Three subsystems are queried and joined on the GDI device name
//! (`\\.\DISPLAY1`), which is the only key all three share:
//!
//! | Source | Gives us |
//! |---|---|
//! | DXGI (`IDXGIOutput6::GetDesc1`) | bit depth, colour space, primaries, luminance, adapter |
//! | CCD (`QueryDisplayConfig` + `DisplayConfigGetDeviceInfo`) | EDID friendly name, connector, exact refresh, HDR *enabled* |
//! | GDI (`EnumDisplaySettingsW`, `GetMonitorInfoW`) | mode list, native mode, primary flag, fallback geometry |
//!
//! Every probe is best-effort. See the robustness note in `lib.rs`.

use crate::DisplayError;
use std::collections::HashMap;
use std::mem::size_of;
use wallset_core::{
    AdapterInfo, ColorCapabilities, Connector, Mode, Monitor, MonitorId, Rect, XyChromaticity,
};
use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
    DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
    DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
    DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME,
    DISPLAYCONFIG_VIDEO_OUTPUT_TECHNOLOGY, DisplayConfigGetDeviceInfo,
    GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
};
use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, LPARAM, RECT};
use windows::Win32::Graphics::Dxgi::Common::DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE,
    DXGI_GPU_PREFERENCE_MINIMUM_POWER, IDXGIAdapter1, IDXGIFactory1, IDXGIFactory6, IDXGIOutput6,
};
use windows::Win32::Graphics::Gdi::{
    DEVMODEW, ENUM_CURRENT_SETTINGS, ENUM_DISPLAY_SETTINGS_MODE, EnumDisplayMonitors,
    EnumDisplaySettingsW, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;
use windows::core::{BOOL, Interface, PCWSTR};

/// What DXGI tells us about one output.
#[derive(Debug, Clone)]
struct DxgiOutput {
    bounds: Rect,
    bits_per_color: u32,
    hdr_colorspace: bool,
    red: [f32; 2],
    green: [f32; 2],
    blue: [f32; 2],
    white: [f32; 2],
    min_lum: f32,
    max_lum: f32,
    max_full_frame_lum: f32,
    adapter: AdapterInfo,
}

/// What the CCD API tells us about one active path.
#[derive(Debug, Clone, Default)]
struct CcdPath {
    friendly_name: Option<String>,
    device_path: Option<String>,
    connector: Option<Connector>,
    refresh_hz: Option<f64>,
    hdr_supported: Option<bool>,
    hdr_enabled: Option<bool>,
    bits_per_channel: Option<u32>,
}

pub fn enumerate() -> Result<Vec<Monitor>, DisplayError> {
    let gdi = gdi_monitors();
    if gdi.is_empty() {
        return Err(DisplayError::NoDisplays);
    }
    // DXGI and CCD failures degrade the report; they do not abort it.
    let dxgi = dxgi_outputs().unwrap_or_default();
    let ccd = ccd_paths().unwrap_or_default();

    let monitors = gdi
        .into_iter()
        .map(|(device, hmon, bounds, primary)| {
            build_monitor(&device, hmon, bounds, primary, dxgi.get(&device), ccd.get(&device))
        })
        .collect();
    Ok(monitors)
}

fn build_monitor(
    device: &str,
    hmon: HMONITOR,
    gdi_bounds: Rect,
    is_primary: bool,
    dx: Option<&DxgiOutput>,
    cc: Option<&CcdPath>,
) -> Monitor {
    let (current, native, max_refresh) = gdi_modes(device);

    // CCD gives the exact rational refresh (59.94); GDI only gives integers.
    let mut current_mode = current.unwrap_or(Mode {
        width: gdi_bounds.width,
        height: gdi_bounds.height,
        refresh_hz: 0.0,
    });
    if let Some(hz) = cc.and_then(|c| c.refresh_hz) {
        current_mode.refresh_hz = hz;
    }

    let (dpi, scale) = monitor_dpi(hmon);

    let color = match dx {
        Some(d) => {
            let xy = |p: [f32; 2]| {
                // Some drivers report all-zero primaries rather than failing.
                (p[0] > 0.0 && p[1] > 0.0).then(|| XyChromaticity::new(p[0], p[1]))
            };
            let lum = |v: f32| (v > 0.0).then_some(v);
            ColorCapabilities {
                bits_per_channel: cc
                    .and_then(|c| c.bits_per_channel)
                    .unwrap_or(d.bits_per_color)
                    .min(u32::from(u8::MAX)) as u8,
                hdr_supported: cc.and_then(|c| c.hdr_supported).unwrap_or(d.hdr_colorspace),
                // DXGI reporting the PQ colour space means HDR is currently on.
                hdr_enabled: cc.and_then(|c| c.hdr_enabled).unwrap_or(d.hdr_colorspace),
                max_luminance_nits: lum(d.max_lum),
                max_full_frame_luminance_nits: lum(d.max_full_frame_lum),
                min_luminance_nits: lum(d.min_lum),
                red: xy(d.red),
                green: xy(d.green),
                blue: xy(d.blue),
                white: xy(d.white),
            }
        }
        None => ColorCapabilities {
            bits_per_channel: cc.and_then(|c| c.bits_per_channel).unwrap_or(8) as u8,
            hdr_supported: cc.and_then(|c| c.hdr_supported).unwrap_or(false),
            hdr_enabled: cc.and_then(|c| c.hdr_enabled).unwrap_or(false),
            max_luminance_nits: None,
            max_full_frame_luminance_nits: None,
            min_luminance_nits: None,
            red: None,
            green: None,
            blue: None,
            white: None,
        },
    };

    let adapter = dx.map(|d| d.adapter.clone()).unwrap_or(AdapterInfo {
        name: "Unknown adapter".into(),
        dedicated_video_memory_bytes: 0,
        shared_system_memory_bytes: 0,
        is_software: false,
    });

    let friendly_name = cc
        .and_then(|c| c.friendly_name.clone())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| {
            // Internal laptop panels frequently have no EDID friendly name.
            if cc.and_then(|c| c.connector).is_some_and(Connector::is_builtin) {
                "Built-in display".into()
            } else {
                "Generic monitor".into()
            }
        });

    Monitor {
        // Device path is EDID-derived and survives re-plugging; the GDI name
        // does not. Fall back to GDI only when CCD failed.
        id: MonitorId(
            cc.and_then(|c| c.device_path.clone())
                .filter(|p| !p.is_empty())
                .unwrap_or_else(|| device.to_string()),
        ),
        friendly_name,
        connector: cc.and_then(|c| c.connector).unwrap_or(Connector::Unknown),
        is_primary,
        bounds: dx.map_or(gdi_bounds, |d| d.bounds),
        current_mode,
        native_mode: native,
        max_refresh_hz: max_refresh,
        dpi,
        scale,
        color,
        adapter,
    }
}

// --- GDI --------------------------------------------------------------------

fn gdi_monitors() -> Vec<(String, HMONITOR, Rect, bool)> {
    unsafe extern "system" fn cb(hmon: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the `&mut Vec<HMONITOR>` passed below and outlives
        // the synchronous EnumDisplayMonitors call.
        let list = unsafe { &mut *(data.0 as *mut Vec<HMONITOR>) };
        list.push(hmon);
        BOOL(1)
    }

    let mut handles: Vec<HMONITOR> = Vec::new();
    // SAFETY: callback and data pointer are valid for the duration of the call.
    let _ = unsafe {
        EnumDisplayMonitors(None, None, Some(cb), LPARAM(&raw mut handles as isize))
    };

    handles
        .into_iter()
        .filter_map(|hmon| {
            let mut info = MONITORINFOEXW::default();
            info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
            // SAFETY: MONITORINFOEXW begins with MONITORINFO and cbSize says so.
            let ok = unsafe {
                GetMonitorInfoW(hmon, (&raw mut info).cast::<MONITORINFO>())
            };
            if !ok.as_bool() {
                return None;
            }
            let device = wide_to_string(&info.szDevice);
            let r = info.monitorInfo.rcMonitor;
            let primary = info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0;
            Some((device, hmon, rect_from(r), primary))
        })
        .collect()
}

/// Returns (current mode, native mode, highest refresh at native resolution).
fn gdi_modes(device: &str) -> (Option<Mode>, Option<Mode>, Option<f64>) {
    let wide: Vec<u16> = device.encode_utf16().chain(std::iter::once(0)).collect();
    let name = PCWSTR(wide.as_ptr());

    let read = |which: ENUM_DISPLAY_SETTINGS_MODE| -> Option<DEVMODEW> {
        let mut dm = DEVMODEW { dmSize: size_of::<DEVMODEW>() as u16, ..Default::default() };
        // SAFETY: `name` is NUL-terminated and alive; dmSize is set.
        unsafe { EnumDisplaySettingsW(name, which, &mut dm) }.as_bool().then_some(dm)
    };
    let to_mode = |dm: &DEVMODEW| Mode {
        width: dm.dmPelsWidth,
        height: dm.dmPelsHeight,
        refresh_hz: f64::from(dm.dmDisplayFrequency),
    };

    let current = read(ENUM_CURRENT_SETTINGS).map(|dm| to_mode(&dm));

    // The native mode is the largest resolution the driver offers. This is
    // what Windows itself labels "(Recommended)" in virtually all cases.
    let mut native: Option<Mode> = None;
    let mut i = 0u32;
    while let Some(dm) = read(ENUM_DISPLAY_SETTINGS_MODE(i)) {
        let m = to_mode(&dm);
        let better = native.is_none_or(|n| {
            let (a, b) = (u64::from(m.width) * u64::from(m.height), u64::from(n.width) * u64::from(n.height));
            a > b || (a == b && m.refresh_hz > n.refresh_hz)
        });
        if better {
            native = Some(m);
        }
        i += 1;
        if i > 4096 {
            break; // Defensive bound against a misbehaving driver.
        }
    }
    let max_refresh = native.map(|n| n.refresh_hz);
    (current, native, max_refresh)
}

fn monitor_dpi(hmon: HMONITOR) -> (u32, f32) {
    let (mut x, mut y) = (0u32, 0u32);
    // SAFETY: valid HMONITOR from EnumDisplayMonitors; out-params are locals.
    match unsafe { GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut x, &mut y) } {
        Ok(()) if x > 0 => (x, x as f32 / 96.0),
        _ => (96, 1.0),
    }
}

// --- DXGI -------------------------------------------------------------------

fn dxgi_outputs() -> windows::core::Result<HashMap<String, DxgiOutput>> {
    // SAFETY: plain COM factory creation.
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
    let mut out = HashMap::new();

    let mut ai = 0u32;
    // SAFETY: EnumAdapters1 returns DXGI_ERROR_NOT_FOUND past the end.
    while let Ok(adapter) = unsafe { factory.EnumAdapters1(ai) } {
        ai += 1;
        let adapter_info = adapter_info(&adapter);

        let mut oi = 0u32;
        // SAFETY: as above, terminates with DXGI_ERROR_NOT_FOUND.
        while let Ok(output) = unsafe { adapter.EnumOutputs(oi) } {
            oi += 1;
            // IDXGIOutput6 exists on Windows 10 1703+. Without it we still
            // have GDI/CCD data, so skip rather than fail.
            let Ok(o6) = output.cast::<IDXGIOutput6>() else { continue };
            // SAFETY: COM call on a live interface.
            let Ok(d) = (unsafe { o6.GetDesc1() }) else { continue };
            if !d.AttachedToDesktop.as_bool() {
                continue;
            }
            let device = wide_to_string(&d.DeviceName);
            out.insert(
                device,
                DxgiOutput {
                    bounds: rect_from(d.DesktopCoordinates),
                    bits_per_color: d.BitsPerColor,
                    hdr_colorspace: d.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020,
                    red: d.RedPrimary,
                    green: d.GreenPrimary,
                    blue: d.BluePrimary,
                    white: d.WhitePoint,
                    min_lum: d.MinLuminance,
                    max_lum: d.MaxLuminance,
                    max_full_frame_lum: d.MaxFullFrameLuminance,
                    adapter: adapter_info.clone(),
                },
            );
        }
    }
    Ok(out)
}

fn adapter_info(adapter: &IDXGIAdapter1) -> AdapterInfo {
    // SAFETY: COM call on a live interface.
    match unsafe { adapter.GetDesc1() } {
        Ok(d) => AdapterInfo {
            name: wide_to_string(&d.Description),
            dedicated_video_memory_bytes: d.DedicatedVideoMemory as u64,
            shared_system_memory_bytes: d.SharedSystemMemory as u64,
            is_software: (d.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) != 0,
        },
        Err(_) => AdapterInfo {
            name: "Unknown adapter".into(),
            dedicated_video_memory_bytes: 0,
            shared_system_memory_bytes: 0,
            is_software: false,
        },
    }
}

/// On a hybrid-graphics machine, the description of the GPU Windows itself
/// classes as minimum-power (the integrated one). `None` on single-GPU
/// systems, where there is no choice to make.
///
/// Uses `EnumAdapterByGpuPreference`, the OS's own answer, rather than
/// matching vendor names: it is correct for Intel and AMD iGPUs alike, and
/// follows the user's per-app graphics settings.
pub fn power_saving_adapter() -> Option<String> {
    // SAFETY: plain COM factory creation and enumeration.
    let f: IDXGIFactory6 = unsafe { CreateDXGIFactory1() }.ok()?;
    let low: IDXGIAdapter1 =
        unsafe { f.EnumAdapterByGpuPreference(0, DXGI_GPU_PREFERENCE_MINIMUM_POWER) }.ok()?;
    let high: IDXGIAdapter1 =
        unsafe { f.EnumAdapterByGpuPreference(0, DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE) }.ok()?;
    let (lo, hi) = (adapter_info(&low), adapter_info(&high));
    (lo.name != hi.name && !lo.is_software).then_some(lo.name)
}

// --- CCD (Connecting and Configuring Displays) ------------------------------

fn ccd_paths() -> Option<HashMap<String, CcdPath>> {
    // The topology can change between sizing and querying, which returns
    // ERROR_INSUFFICIENT_BUFFER. Retry a few times, as Microsoft's own samples do.
    let (paths, _modes) = (0..4).find_map(|_| {
        let (mut np, mut nm) = (0u32, 0u32);
        // SAFETY: out-params are locals.
        if unsafe { GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm) }
            != ERROR_SUCCESS
        {
            return None;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
        // SAFETY: buffers are sized to the counts passed in.
        let rc = unsafe {
            QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut np,
                paths.as_mut_ptr(),
                &mut nm,
                modes.as_mut_ptr(),
                None,
            )
        };
        if rc == ERROR_INSUFFICIENT_BUFFER {
            return None;
        }
        (rc == ERROR_SUCCESS).then(|| {
            paths.truncate(np as usize);
            modes.truncate(nm as usize);
            (paths, modes)
        })
    })?;

    let mut out = HashMap::new();
    for p in &paths {
        let src = &p.sourceInfo;
        let tgt = &p.targetInfo;

        let Some(gdi_name) = source_gdi_name(src.adapterId, src.id) else { continue };

        let mut c = CcdPath::default();

        let rr = tgt.refreshRate;
        if rr.Denominator != 0 && rr.Numerator != 0 {
            c.refresh_hz = Some(f64::from(rr.Numerator) / f64::from(rr.Denominator));
        }

        let mut tn = DISPLAYCONFIG_TARGET_DEVICE_NAME {
            header: header(
                DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
                size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>(),
                tgt.adapterId,
                tgt.id,
            ),
            ..Default::default()
        };
        // SAFETY: header describes the struct it heads.
        if unsafe { DisplayConfigGetDeviceInfo(&mut tn.header) } == ERROR_SUCCESS.0 as i32 {
            c.friendly_name = Some(wide_to_string(&tn.monitorFriendlyDeviceName));
            c.device_path = Some(wide_to_string(&tn.monitorDevicePath));
            c.connector = Some(map_connector(tn.outputTechnology));
        }

        let mut ac = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO {
            header: header(
                DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
                size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>(),
                tgt.adapterId,
                tgt.id,
            ),
            ..Default::default()
        };
        // SAFETY: as above.
        if unsafe { DisplayConfigGetDeviceInfo(&mut ac.header) } == ERROR_SUCCESS.0 as i32 {
            // SAFETY: reading the plain u32 view of a bitfield union.
            let bits = unsafe { ac.Anonymous.value };
            c.hdr_supported = Some(bits & 0b01 != 0);
            c.hdr_enabled = Some(bits & 0b10 != 0);
            if ac.bitsPerColorChannel > 0 {
                c.bits_per_channel = Some(ac.bitsPerColorChannel);
            }
        }

        out.insert(gdi_name, c);
    }
    Some(out)
}

fn source_gdi_name(adapter: windows::Win32::Foundation::LUID, id: u32) -> Option<String> {
    let mut sn = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
        header: header(
            DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
            size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>(),
            adapter,
            id,
        ),
        ..Default::default()
    };
    // SAFETY: header describes the struct it heads.
    (unsafe { DisplayConfigGetDeviceInfo(&mut sn.header) } == ERROR_SUCCESS.0 as i32)
        .then(|| wide_to_string(&sn.viewGdiDeviceName))
}

fn header(
    kind: windows::Win32::Devices::Display::DISPLAYCONFIG_DEVICE_INFO_TYPE,
    size: usize,
    adapter: windows::Win32::Foundation::LUID,
    id: u32,
) -> DISPLAYCONFIG_DEVICE_INFO_HEADER {
    DISPLAYCONFIG_DEVICE_INFO_HEADER { r#type: kind, size: size as u32, adapterId: adapter, id }
}

fn map_connector(t: DISPLAYCONFIG_VIDEO_OUTPUT_TECHNOLOGY) -> Connector {
    // Raw values from wingdi.h; the high bit marks an internal connection.
    match t.0 {
        v if v as u32 == 0x8000_0000 => Connector::Internal,
        0 => Connector::Vga,
        4 => Connector::Dvi,
        5 => Connector::Hdmi,
        6 | 13 => Connector::Internal, // LVDS, UDI embedded
        10 => Connector::DisplayPort,
        11 => Connector::EmbeddedDisplayPort,
        15 => Connector::Wireless, // Miracast
        16 | 17 => Connector::Virtual, // Indirect wired / virtual
        18 => Connector::Usb4,     // DisplayPort over USB tunnel
        _ => Connector::Unknown,
    }
}

// --- helpers ----------------------------------------------------------------

fn rect_from(r: RECT) -> Rect {
    Rect {
        x: r.left,
        y: r.top,
        width: (r.right - r.left).max(0) as u32,
        height: (r.bottom - r.top).max(0) as u32,
    }
}

fn wide_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_strings_stop_at_the_first_nul() {
        let buf: Vec<u16> = "DELL\0garbage".encode_utf16().collect();
        assert_eq!(wide_to_string(&buf), "DELL");
    }

    #[test]
    fn connector_mapping_covers_common_outputs() {
        assert_eq!(map_connector(DISPLAYCONFIG_VIDEO_OUTPUT_TECHNOLOGY(5)), Connector::Hdmi);
        assert_eq!(map_connector(DISPLAYCONFIG_VIDEO_OUTPUT_TECHNOLOGY(10)), Connector::DisplayPort);
        assert_eq!(
            map_connector(DISPLAYCONFIG_VIDEO_OUTPUT_TECHNOLOGY(11)),
            Connector::EmbeddedDisplayPort
        );
    }

    /// Runs against the real hardware. Not a unit test in the strict sense,
    /// but it is the only way to know the FFI joins actually line up.
    #[test]
    fn enumerates_at_least_one_real_monitor() {
        let mons = enumerate().expect("a Windows test machine has a display");
        assert!(!mons.is_empty());
        assert!(mons.iter().all(|m| m.current_mode.width > 0 && m.current_mode.height > 0));
    }
}
