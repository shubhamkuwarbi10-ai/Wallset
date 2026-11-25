# wallSet

An image and video wallpaper setter built around one rule: **a wallpaper
nobody can see should cost nothing.**

* Pick any image or video, see whether it suits your display, preview it with
  the real renderer, and apply it per monitor.
* See what each display actually is: connector, exact refresh rate, native
  mode, scaling, bit depth, HDR (supported *and* whether it is switched on),
  brightness in nits, and colour gamut computed from the panel's reported
  primaries.
* Video plays only while you can see it. Covered by windows: **paused, 0%
  CPU**. Covered for a while, a fullscreen game, monitor off, or low battery:
  **the renderer is terminated, 0 MB**, and its last frame stays as a still
  wallpaper, so the desktop never goes black.

## Status

| | Windows 10/11 | macOS | Linux |
|---|---|---|---|
| Display capability probing | **Working** | stub | stub |
| Still-image wallpaper | **Working** (per monitor) | stub | stub |
| Video wallpaper | **Working** (incl. 24H2+ desktop layout) | stub | stub |
| Background service + policy | **Working** | — | — |
| Settings UI with live preview | **Working** | builds, no backend | builds, no backend |

The macOS and Linux backends are stubs that report "not implemented" rather
than fabricating data. Their intended mechanisms, including why video
wallpaper is **not possible on GNOME under Wayland**, are in
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) section 2.

## Measured cost

From [docs/BENCHMARKS.md](docs/BENCHMARKS.md) (one laptop: RTX 3060 + Intel
UHD, 1080p 144 Hz):

| State | Cost |
|---|---|
| Background service, any state | **2.4 MB** private memory, **0.00%** CPU |
| Video covered by windows (paused) | **0.00%** CPU |
| Video suspended (covered > 90 s, fullscreen app, ...) | renderer **0 MB, 0%** |
| Video visible on screen, 1080p 24 fps | **~4.6%** of one CPU core, **~260 MB**, 7.2% GPU (iGPU, full screen; BENCHMARKS section 6). ~340 MB on the dGPU |
| Video visible, 4K 10-bit HDR file | ~1.05–1.2 GB. Use a screen-sized copy; the app tells you how |
| Settings window open | ~270 MB, **0 when closed** |

Most of the visible-playback memory is the GPU driver's per-process cost of
a Direct3D device (~180 MB here), which any GPU-rendered wallpaper pays.
The largest cost you control is the **source resolution**: a 4K file on a
1080p screen costs ~400 MB more than a 1080p file and looks no sharper.

## Build

Windows, Rust stable with the **MSVC** toolchain (the GNU toolchain cannot
generate `windows-rs` import libraries without a full MinGW-w64):

```powershell
rustup set default-host x86_64-pc-windows-msvc
winget install --id Microsoft.VisualStudio.BuildTools --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
cargo test --workspace
.\tools\stage.ps1          # -> dist\wallSet\ with libmpv alongside
```

libmpv is not committed. Put `libmpv-2.dll` in `vendor\mpv\` (the version
used for the benchmarks is recorded in `vendor\mpv\SOURCE.txt`), or point
`WALLSET_LIBMPV` at it during development.

## Use

```powershell
dist\wallSet\wallset-ui.exe                    # settings window
dist\wallSet\wallset-ui.exe clip.mp4 --preview # open with a file, preview it
dist\wallSet\wallset.exe displays              # what each display supports
dist\wallSet\wallset.exe check clip.mp4        # will it look right? what will it cost?
dist\wallSet\wallset.exe caps                  # what this session supports
```

The settings window starts the background service on first apply. The
service lives in the notification area: settings, "Pause video
wallpapers", quit. It logs every decision with its reason to
`%LOCALAPPDATA%\wallSet\daemon.log`.

## Layout

```
crates/
  wallset-core      Policy state machine, suitability check, cost model, config. No platform code, no unsafe.
  wallset-display   Display probing (DXGI + CCD + GDI on Windows).
  wallset-surface   Desktop layer (WorkerW), per-monitor still wallpaper, process efficiency and stats.
  wallset-media     libmpv binding (loaded at runtime), media probe, shared render profile.
  wallset-render    [bin] Short-lived renderer: one video, one window.
  wallset-daemon    [bin] Resident service. Event-driven; never polls, never renders.
  wallset-ui        [bin] Settings window. Exits when closed.
  wallset-cli       [bin] `wallset`: diagnostics, measurement, scripting.
tools/
  bench.py          Process benchmark (private bytes, working set, precise CPU, GPU engines). Stdlib only.
  stage.ps1         Assemble a runnable folder.
docs/
  ARCHITECTURE.md   Design, per-OS feasibility, policy tiers, risks.
  BENCHMARKS.md     All measurements, method, caveats, open questions.
```

## Known limitations

* Visible-playback CPU is measured for one case only (1080p 24 fps, iGPU).
  60 fps, 4K and the dGPU still need visible, full-screen runs.
* All measurements come from one machine.
* The full service lifecycle (start on reveal, pause on cover, free after
  the grace period, restart on reveal) is verified with the desktop actually
  visible. The tray menu's "Pause" and lock/unlock and battery transitions
  are covered by unit tests but not yet exercised live.
* Wallpaper engines that also use the desktop layer (e.g. Lively Wallpaper)
  will compete with wallSet for it. Pause the other one.
* No installer or branded icon yet.

## Licence

wallSet: MPL-2.0. libmpv is GPLv2+ / LGPLv2.1+ depending on build
configuration; check the licence of the build you ship.
