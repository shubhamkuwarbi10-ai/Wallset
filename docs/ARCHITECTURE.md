# wallSet — Architecture & Feasibility

A cross-platform wallpaper setter for image and video media, designed so that
"lightweight" is a measured property rather than a claim.

---

## 1. Success criteria

The brief ("eats less RAM, minimal CPU and GPU") is not testable as written.
These are the numbers the implementation is held to. Every one is measurable
with Process Explorer / `pidstat` / Instruments, and the repo ships a harness
that records them (`docs/BENCHMARKS.md`).

| State | Memory budget (private bytes) | CPU budget | GPU budget |
|---|---|---|---|
| Idle, static image wallpaper set | **< 10 MB** | **0.0 %** (truly zero; no timer) | 0 % |
| Video wallpaper, desktop fully visible, 1080p | < 400 MB *(revised; was < 110 MB)* | < 5 % of one core *(revised; was < 2.5 %)* | not yet measured |
| Video wallpaper, covered by windows | as above | **0.0 %** | 0 % |
| Video wallpaper, fullscreen app in foreground | **0 MB** renderer, < 10 MB daemon | **0.0 %** | 0 % |
| Config UI open | transient, not resident | reactive only | — |

**Revision note (2026-11-25).** The original visible-playback figures were
set before any measurement and were wrong by ~3x for memory. Measurement
showed the floor is the GPU driver's per-process cost of a D3D11 device
(~180 MB private on the reference NVIDIA driver), which no player
configuration removes. The zero-cost states, which are the architectural
claim, were met exactly. Full data, method and the original-vs-measured
table: [BENCHMARKS.md](BENCHMARKS.md).

The two bolded zeroes are the whole design. A wallpaper is, by definition,
invisible most of the time it is "running". An engine that spends resources
while nobody can see it has failed regardless of how well-optimised its
decoder is.

### Why not Electron / Tauri-webview

Approximate baselines for an empty window with no content:

- Electron: ~120-300 MB RSS, 3+ processes, Chromium compositor always resident.
- Tauri (WebView2 / WebKitGTK): ~80-180 MB while a window is open.
- Rust + native toolkit: ~5-8 MB for a message-loop-only daemon.

A 120 MB floor is larger than the *entire* budget for the video-playing state.
This rules out webview shells for the resident process. It does **not** rule
them out for the config UI, which is why the UI is a separate, non-resident
process (section 3) — but since we need native platform code anyway, a native
UI costs little extra.

---

## 2. Platform feasibility — the honest table

"Every common OS" holds for static images. It does **not** hold uniformly for
video, and the gaps are architectural, not effort-related. Knowing this now is
worth more than discovering it in month three.

| Platform | Static image | Video wallpaper | Mechanism |
|---|---|---|---|
| Windows 10/11 | Yes | **Yes** | `SystemParametersInfoW(SPI_SETDESKWALLPAPER)`; for video, send undocumented message `0x052C` to `Progman` to spawn a `WorkerW` window, then reparent the render surface into the `WorkerW` that sits behind the desktop icons |
| macOS 11+ | Yes | **Yes** | `NSWorkspace.setDesktopImageURL`; for video, an `NSWindow` at `CGWindowLevelForKey(.desktopWindow)` with `canJoinAllSpaces` + `stationary`, `ignoresMouseEvents = true` |
| Linux / X11 (any DE) | Yes | **Yes** | Override-redirect window with `_NET_WM_WINDOW_TYPE_DESKTOP`, or draw to the root pixmap via `_XROOTPMAP_ID` |
| Linux / wlroots Wayland (Sway, Hyprland, river) | Yes | **Yes** | `wlr-layer-shell-unstable-v1`, background layer — the same protocol `swaybg` and `mpvpaper` use |
| Linux / KDE Plasma | Yes | **Qualified** | Plasma owns the desktop surface; a foreign window cannot go behind it. Requires shipping a QML wallpaper plugin into `~/.local/share/plasma/wallpapers/` — a genuinely different deliverable from the rest of the codebase |
| Linux / GNOME Wayland | Yes | **No** | Mutter exposes no background layer to third-party clients and there is no layer-shell equivalent. Would require a GNOME Shell extension (JS, versioned against each GNOME release, breaks roughly every 6 months). Out of scope |

**Consequence for the roadmap.** Video wallpaper is a Windows + macOS + X11 +
wlroots feature. GNOME-Wayland users get static images and an explicit,
honest in-app explanation rather than a silently broken feature. This is a
scope decision made deliberately, not a defect.

---

## 3. Process model and the resource policy engine

### Three processes, two of them usually absent

```
wallset-daemon        resident, always.    ~5-8 MB. Message loop, blocked in GetMessage().
  |- wallset-render   spawned on demand.   ~90 MB while decoding. TERMINATED when invisible.
  |- wallset-ui       spawned on demand.   ~60 MB. Exits fully on window close.
```

The daemon never decodes, never renders, never polls. It owns configuration,
display topology, and the policy state machine. It is blocked in a blocking
OS call whenever it is not reacting to an event, which is how the 0.0 % idle
figure is achieved — not by being fast, but by not running.

### The policy state machine

This is the central mechanism. Visibility is not binary, so neither is the
response:

| Tier | Trigger | Action | RAM | CPU |
|---|---|---|---|---|
| **T0 Visible** | Desktop reachable by the user | Decode and present at the monitor's refresh rate, capped at source fps | ~90 MB | ~2 % |
| **T1 Occluded** | Any non-fullscreen window covers the monitor; or screensaver active | `pause` the render process. Process stays warm; resume is instant | ~90 MB | **0 %** |
| **T2 Suspended** | Fullscreen app in foreground; monitor powered off; session locked > 60 s; occluded > 90 s; on battery below the user's threshold | **Terminate** the render process. Last frame stays as a static desktop image, so the desktop does not go black | **0 MB** | **0 %** |

T1 exists because terminate/respawn costs ~250-400 ms and loses playback
position — unacceptable for a window you alt-tab away from for two seconds.
T2 exists because a paused process still holds ~90 MB of decoded-frame pools,
and that is unacceptable while the user is gaming. The 90-second T1 to T2
timer is the compromise, and it is user-configurable.

T2's "freeze the last frame as a static wallpaper" detail matters: the
expensive path shuts down entirely without the user ever seeing the
degradation.

### Everything is event-driven. Nothing is polled.

Polling is the single largest cause of idle CPU in background apps. A 1 Hz
poll loop is not free — it prevents deep C-states and measurably costs
battery. Windows signal sources, all push-based:

| Event | API |
|---|---|
| Foreground window changed | `SetWinEventHook(EVENT_SYSTEM_FOREGROUND)` |
| Fullscreen / presentation app running | `SHQueryUserNotificationState()` returning `QUNS_RUNNING_D3D_FULL_SCREEN` or `QUNS_PRESENTATION_MODE` |
| Window cloaked (virtual desktop switch) | `DwmGetWindowAttribute(DWMWA_CLOAKED)` |
| Monitor topology or resolution changed | `WM_DISPLAYCHANGE`, `WM_DPICHANGED` |
| Monitor powered off | `RegisterPowerSettingNotification(GUID_SESSION_DISPLAY_STATUS)` |
| AC to battery transition | `RegisterPowerSettingNotification(GUID_ACDC_POWER_SOURCE)` |
| Battery level crossed threshold | `GUID_BATTERY_PERCENTAGE_REMAINING` |
| Session lock / unlock / fast user switch | `WTSRegisterSessionNotification` |

The only timer in the daemon is the T1 to T2 countdown, and it is a one-shot
`SetTimer` cancelled the moment the desktop becomes visible again.

---

## 4. Media pipeline

### libmpv, loaded dynamically

libmpv is the video engine for decode, demux, hardware acceleration and
format coverage. Rationale over hand-rolling FFmpeg:

- Hardware decode negotiation (D3D11VA / VideoToolbox / VAAPI) with correct
  per-driver fallback is thousands of lines of fiddly code. mpv has solved it
  in production for fifteen years.
- `--hwdec=auto-safe` keeps decoded frames in GPU memory and presents them
  without a CPU round-trip. This is the difference between roughly 2 % and
  25 % CPU on 4K content, and it is also most of the RAM difference.
- The same library answers the media-probing questions (section 6) that the
  display fitness check needs. One dependency, two jobs.

It is loaded with `libloading` at runtime rather than linked. This means no
MSVC import library is needed (which is what makes the GNU toolchain viable),
and a missing libmpv degrades to "video unavailable, images still work"
instead of a process that will not start.

### Render process, not render thread

The render surface runs in a child process (`mpv --wid=<HWND>` embedding into
the `WorkerW` child). Reasons:

1. A GPU driver crash or a malformed file kills the renderer, not the daemon.
   The daemon notices the exit and either restarts or falls back to a static
   frame. The user's wallpaper never disappears permanently.
2. T2 suspension can reclaim **all** of the memory, which is impossible if
   decoding lives in the daemon's address space — allocator free lists and
   frame pools do not return memory to the OS reliably.

The cost is an IPC hop (mpv's JSON IPC over a named pipe) and about 1 MB of
process overhead. Worth it.

### Cost controls, and what they can and cannot do

All of these live in one place, `wallset-media/src/render.rs`, which both the
preview and the wallpaper apply. That is what makes the preview truthful.

- **Decode resolution cannot be reduced at runtime.** Decoders allocate
  frames at the source's resolution; a 4K file on a 1080p monitor holds 4K
  frame buffers no matter what. Adding a `vf=scale` filter would make things
  *worse*: it forces hardware frames to be copied back to the CPU. The only
  real fix is a screen-sized file, so the fitness check (section 6) measures
  the waste and hands the user the exact `ffmpeg` command to make one.
- **Scaling to the screen is done on the GPU with cheap filters.** mpv's
  `profile=fast` uses bilinear scaling, no dithering and no debanding.
  At wallpaper viewing distance this looks the same and costs far less GPU
  time at 4K.
- **Presentation follows the source frame rate.** With no `video-sync=display-*`
  mode, mpv presents each decoded frame once, so 24 fps content on a 144 Hz
  panel costs 24 presents a second, not 144. Sources faster than the panel
  are dropped at present time; their decode cost remains, and the fitness
  check flags them.
- **The demuxer cache is capped at 8 MiB** (`cache=no`,
  `demuxer-max-bytes=8MiB`). mpv's default read-ahead of up to 150 MiB is
  sized for network streams and is pure waste for a local looping file.
- **Audio is never decoded** (`aid=no`, `ao=null`). That is different from
  muting it, which still decodes and discards.

---

## 5. Display capability probing

Answers the brief's "show the user what their monitor supports and what kind
of display they have". Per connected monitor:

| Property | Windows source |
|---|---|
| Friendly name ("Dell U2720Q", not `\\.\DISPLAY1`) | `QueryDisplayConfig` + `DISPLAYCONFIG_TARGET_DEVICE_NAME` |
| Connector type (HDMI / DisplayPort / internal eDP) | `DISPLAYCONFIG_TARGET_DEVICE_NAME.outputTechnology` |
| Native resolution and current mode | `EnumDisplaySettingsW`, `DEVMODEW` |
| Refresh rate (exact rational, e.g. 59.94 not 60) | `DISPLAYCONFIG_PATH_TARGET_INFO.refreshRate` |
| DPI and scale factor | `GetDpiForMonitor(MDT_EFFECTIVE_DPI)` |
| Bit depth per colour channel | `IDXGIOutput6::GetDesc1().BitsPerColor` |
| Colour gamut (CIE primaries and white point) | `IDXGIOutput6::GetDesc1()` red/green/blue primaries |
| HDR **supported** | `GetDesc1().ColorSpace == RGB_FULL_G2084_NONE_P2020` |
| HDR **currently enabled** (a different question) | `DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO.advancedColorEnabled` |
| Peak and sustained luminance (nits) | `GetDesc1().MaxLuminance`, `MaxFullFrameLuminance` |
| GPU driving the output | `IDXGIAdapter1::GetDesc1().Description`, `DedicatedVideoMemory` |

Separating "HDR supported" from "HDR enabled" matters: a user with an HDR
monitor and HDR toggled off in Windows settings sees washed-out PQ content,
and we can warn them specifically instead of vaguely.

macOS equivalents: `NSScreen.frame` / `backingScaleFactor`,
`maximumPotentialExtendedDynamicRangeColorComponentValue` (EDR headroom),
`CGDisplayCopyDisplayMode`, `CGDisplayCopyColorSpace`.
Linux: XRandR properties, `wl_output` plus `xdg-output`, and DRM connector
properties (`HDR_OUTPUT_METADATA`, `max_bpc`) for HDR.

---

## 6. Media fitness check

Probing the display is only half the feature. The useful output is the
*comparison*. On media selection, mpv is loaded paused with `--vo=null` and
queried, then cross-checked against the target monitor:

| Check | Source property | Warning raised when |
|---|---|---|
| Hardware decode available | `hwdec-current` | returns `no` — "this file will be decoded on the CPU; expect roughly 20 % CPU usage" |
| Codec support | `video-codec`, `video-format` | AV1 on pre-2020 GPUs, HEVC without the Windows extension |
| Resolution mismatch | `video-params/w,h` vs monitor mode | source below 70 % of monitor width — visible upscaling blur |
| Wasted resolution | same | source at or above 2x monitor width — "downscaling 4K to 1080p; a 1080p copy would use half the memory" |
| Frame rate vs refresh | `container-fps` vs refresh | non-integer ratio causes judder, e.g. 30 fps on a 50 Hz panel |
| HDR mismatch | `video-params/gamma` is `pq` or `hlg` | HDR source on an SDR display, or on an HDR display with HDR disabled — tonemapping warning |
| Colour gamut | `video-params/primaries` | BT.2020 source on an sRGB panel |
| File size and duration | `duration`, file stat | very long files mean large seek tables; very short means a visible loop seam |

### Preview uses the real pipeline

The preview window embeds the **same** mpv process configuration the
wallpaper will use — same hwdec, same scaling, same fps cap — rendered into a
child window. A preview using a different code path (an FFmpeg thumbnail,
say) can show something the wallpaper will not reproduce, which makes the
feature actively misleading. The preview also displays live CPU and RAM
figures for the preview process, so the user sees the cost of a file before
committing to it.

---

## 7. Repository layout

```
wallSet/
|- crates/
|  |- wallset-core/      Config, media model, policy state machine. Zero platform code.
|  |- wallset-display/   Display capability probing.    platform/{windows,macos,linux}.rs
|  |- wallset-surface/   Wallpaper surface acquisition. platform/{windows,macos,linux}.rs
|  |- wallset-media/     libmpv binding, probing, fitness analysis.
|  |- wallset-daemon/    [bin] Resident tray process + policy loop.
|  |- wallset-ui/        [bin] On-demand config window.
|- docs/
```

`wallset-core` compiles on every platform and contains the policy state
machine, which is therefore unit-testable in CI without a display server —
the part most likely to harbour logic bugs is the part with no platform
dependencies.

Platform backends sit behind two traits:

```rust
trait DisplayProbe {
    fn enumerate(&self) -> Result<Vec<Monitor>>;
}

trait WallpaperSurface {
    fn set_static(&mut self, path: &Path, mon: MonitorId) -> Result<()>;
    fn acquire_video_surface(&mut self, mon: MonitorId) -> Result<RawSurfaceHandle>;
    fn release(&mut self, mon: MonitorId) -> Result<()>;
    fn capabilities(&self) -> SurfaceCapabilities; // reports "no video here" for GNOME-Wayland
}
```

`capabilities()` is what makes section 2's honest table a runtime behaviour
instead of a README footnote.

---

## 8. Known risks

| Risk | Severity | Mitigation |
|---|---|---|
| `WorkerW` message `0x052C` is undocumented and has shifted between Windows builds | High | Probe and verify the window hierarchy after the call; fall back to static wallpaper if `WorkerW` is not found. Never assume it worked |
| Explorer restarts (crash or update) and destroys our `WorkerW` parent | Medium | Watch for the `TaskbarCreated` registered message; re-acquire the surface |
| GNOME-Wayland users expect video and find it absent | Medium | `capabilities()` drives the UI; the video tab explains why, with the Mutter reason, rather than failing silently |
| libmpv absent or ABI-mismatched | Medium | Dynamic load with a version check; degrade to images-only with a clear message |
| mpv subprocess leaks on daemon crash | Medium | Pass a Win32 Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` so the OS reaps children |
| Multi-monitor with mixed DPI or HDR | Medium | Per-monitor surface and per-monitor render process; never one stretched surface |
| HDR video tonemapped to SDR looks washed out | Low | Detected in section 6 and surfaced before the user commits |
| GNU toolchain cannot link windows-rs | *Occurred* | windows-rs 0.61+ generates import libraries via raw-dylib, which on the GNU host needs a full x86-64 MinGW assembler. Resolved by building with MSVC |
| MSIX file-system virtualisation | *Occurred*, fixed | Under a packaged parent (or if wallSet ships as MSIX), AppData writes are redirected to a per-package folder that Explorer cannot read, so freeze frames failed to apply. `set_static_wallpaper` retries from `%TEMP%` on "file not found" for an existing file. See BENCHMARKS.md section 6 |
| Another wallpaper engine in the same desktop layer (e.g. Lively) | Medium | Both insert below `SHELLDLL_DefView`; whichever inserted last is on top. Detect foreign windows in the layer and tell the user (not yet implemented) |
