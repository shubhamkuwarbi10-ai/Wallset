# wallSet — Benchmarks

Measured, not modelled. Every number here comes from a real run on the
reference machine below. When the model in `wallset-core/src/fitness.rs`
disagrees with this file, this file wins and the model is recalibrated; the
test `model_reproduces_the_benchmark_measurements` enforces that.

## Reference machine

| | |
|---|---|
| OS | Windows 11, build 26300 (24H2+ "Raised" desktop layout) |
| Display | Built-in 1920x1080, 144.15 Hz, 125% scaling, 8-bit SDR, ~62% sRGB |
| GPUs | NVIDIA GeForce RTX 3060 Laptop (driver 32.0.16.1692), drives the panel; Intel UHD Graphics (driver 30.0.101.2079) |
| libmpv | 2.6, shinchiro build 20261111 (`vendor/mpv/SOURCE.txt`) |
| Build | `cargo build --release` (opt-level z, fat LTO) |
| Date | 2026-11-25 |

Test media are synthetic (`ffmpeg -f lavfi -i testsrc2`), so results are
reproducible without distributing video files. Recipes are in
[Reproducing](#reproducing).

## Method

* Tool: `wallset measure <file> --seconds 10 [--mpv-opt k=v ...]`. Spawns the
  real renderer behind the desktop icons, waits for the first frame, lets it
  settle for 4 s, then measures for 10 s.
* **Memory** is reported as **private bytes** (committed memory) and working
  set. Private bytes is the primary metric: working set can be shrunk
  cosmetically by trimming, private bytes cannot.
* **CPU** is the CPU-time delta over the whole 10 s window, as a percentage of
  one core. See [Measurement bug found and fixed](#measurement-bug-found-and-fixed).
* One variable changed per run. Key configurations run twice; CPU noise
  between identical runs is about ±0.6 points.
* Not measured yet: GPU utilisation of the renderer, power draw. See
  [Open questions](#open-questions).

## Results

### 1. Lifecycle (policy tiers) — `wallset demo`

1080p 24 fps H.264, original profile (`gpu-next`, default swapchain), NVIDIA.

| Tier | Renderer CPU | Renderer private | Target | Met? |
|---|---|---|---|---|
| T0 playing | 3.1% | 474 MB peak | < 2.5%, < 110 MB | No / No |
| T1 paused | **0.00%** | 324 MB | 0% CPU | **Yes** |
| T2 suspended | **0** | **0 MB** | 0 / 0 | **Yes** |
| Host process (daemon stand-in) | — | 2.2 MB | < 10 MB | **Yes** |

First frame after spawn: ~0.3–1.0 s. Freeze-to-exit: 461 ms.

The tier mechanism works as designed. The T0 targets were wrong; section 3
explains why.

### 2. What the T0 memory is made of

1080p 24 fps H.264, NVIDIA, one change per row.

| Configuration | CPU* | Private | Working set | Isolates |
|---|---|---|---|---|
| `vo=null hwdec=no` | 15.4% | **57 MB** | 61 MB | libmpv + FFmpeg + software decode; no GPU at all |
| `vo=null` (hw decode only) | 12.3% | 240 MB | 123 MB | a D3D11 device for decoding |
| `hwdec=no` (render only) | 17.2% | 240 MB | 186 MB | a D3D11 device for rendering |
| defaults (`gpu-next`) | 5.5–5.9% | 352 MB | 208 MB | both |
| `gpu-api=vulkan` | 3.1% | 972 MB | 563 MB | — rejected |

\* CPU in this table was measured before the quantisation fix and is only
reliable to ~1.5 points; the memory columns are unaffected.

**Finding:** creating a D3D11 device costs **~180 MB of private memory** on
this NVIDIA driver before any frame is drawn. It is the floor of any
GPU-accelerated wallpaper, including Chromium/WebView2-based players. No mpv
option removes it.

### 3. Renderer configuration (precise CPU, 2 runs each)

| Configuration | CPU | Private | Working set | First frame |
|---|---|---|---|---|
| defaults (`gpu-next`), NVIDIA | 5.46 / 5.93% | 351 / 353 MB | 208 MB | ~560 ms |
| `vo=gpu`, NVIDIA | 4.22 / 3.43% | 357 MB | 211 MB | ~530 ms |
| `vo=gpu swapchain-depth=1`, NVIDIA | 4.84 / 3.75% | 339 / 341 MB | 200 MB | ~530 ms |
| `vo=gpu swapchain-depth=1`, **Intel iGPU** | **2.50 / 1.87%** | **261 MB** | 267 MB | **~300 ms** |

**Decisions taken** (now the defaults in `wallset-media/src/render.rs`):

1. `vo=gpu` instead of `gpu-next`: about 30% less CPU, consistently.
2. `swapchain-depth=1`: −17 MB private, CPU within noise.
3. Render on the power-saving GPU on hybrid systems, chosen through
   `IDXGIFactory6::EnumAdapterByGpuPreference(MINIMUM_POWER)` rather than by
   vendor name: −80 MB private, ~−45% CPU, faster first frame. Cost: +65 MB
   working set from the cross-adapter copy, since the panel is wired to the
   NVIDIA GPU.

### 4. Scaling with resolution, bit depth and frame rate

`vo=gpu swapchain-depth=1`.

| File | NVIDIA private | Intel private | NVIDIA CPU | Intel CPU |
|---|---|---|---|---|
| 1080p 24 fps H.264 8-bit | 338 MB | 261 MB | 3.59% | 3.12% |
| 1080p 60 fps H.264 8-bit | 344 MB | 270 MB | 5.31% | 4.37% |
| 4K 24 fps H.264 8-bit | 757 MB | 637 MB | 3.12% | 7.80% ⚠ |
| 4K 60 fps HEVC 10-bit HDR10 | 1192 MB | 1049 MB | 4.84% | 2.50% |

**Findings:**

* **Source resolution is the largest controllable cost.** 1080p to 4K adds
  ~420 MB; 10-bit adds roughly as much again. Render buffers are sized to the
  *source*, not the screen. This is why the fitness check now treats an
  oversized source as a **Warning** and prints a tested `ffmpeg` command to
  make a screen-sized copy.
* **Linear fit (NVIDIA):** private ≈ 198 MB + 67.4 MB per source megapixel,
  ×2 for >8-bit sources. Predicts the 4K 10-bit file within ~10% (high).
* **Under hardware decode, CPU tracks frame rate, not resolution:** about
  2.45% + 0.047% per presented fps (NVIDIA).

### 5. Caveat: sections 2–4 were measured with the wallpaper *covered*

During every `wallset measure` run above, the user's own windows covered
the desktop, so the wallpaper window was never actually on screen. Windows
skips part of the presentation work for a fully occluded window. The
settings UI's preview (same renderer, same profile, **actually visible**)
then measured:

| File, Intel iGPU | Covered (§3–4) | Visible (preview) |
|---|---|---|
| 1080p H.264 (24 / 25 fps) | 1.9–3.1% CPU, 261 MB | **7.2% CPU**, 258 MB |
| 4K 60 fps HEVC 10-bit HDR | 2.5% CPU, 1049 MB | **10.9% CPU**, 1048 MB |

Memory agrees within 1%; **CPU is 2.5–4x higher when visible.** The two
"visible" figures come from the preview window (smaller than the screen,
inside a foreground app), with short windows of 7 s and 24 s, so they are
indicative rather than conclusive. They are still enough to say that the
CPU figures in sections 3–4 are likely **underestimates** of real visible
playback, and that visible 1080p playback probably exceeds the 5% CPU budget
on this machine.

**Resolved in part by section 6:** a full-screen, genuinely visible session
measured 4.53–4.61% for the 1080p clip on the iGPU. Covered runs understate
visible CPU by about 2x; the preview overstates it. Sections 3–4 CPU figures
remain useful for *comparing* configurations against each other, not as
absolute costs. Visible runs at 60 fps, at 4K and on the NVIDIA GPU are
still to do. The T1/T2 results (0% CPU, 0 MB) are unaffected: they do not
depend on presentation.

### 6. Visible-desktop session (resolves section 5)

2026-11-25. The full service running unattended: `wallset-daemon` with the
1080p 24 fps H.264 clip assigned, 20 s covered-grace, rendering on the Intel
iGPU. The user showed the desktop (Win+D), then restored a maximised window.
Samples come from `tools/bench.py` (1 s interval, 150 s) and were split by
the state transitions the daemon itself logged (`tools/session_report.py`),
discarding 5 s after each transition.

| State (from daemon log) | Measured for | Renderer | Daemon |
|---|---|---|---|
| Visible, run 1 | 10 s | **4.53% CPU**, 256 MB private, 261 MB WS, GPU 7.2% (VideoDecode 3.1%) | 2.4 MB, 0.00% |
| Visible, run 2 (new renderer) | 22 s | **4.61% CPU**, 261 MB private, 267 MB WS, GPU 7.2% (VideoDecode 3.0%) | 2.5 MB, 0.00% |
| Covered, paused | 13 s | **0.00% CPU**, 261 MB held | 2.4 MB, 0.00% |
| Covered > 20 s, freed | 80 s | **not running: 0 MB, 0%** | 2.6 MB, 0.00% |

**Findings:**

* **Visible full-screen 1080p CPU is ~4.6%**, inside the revised 5% budget.
  It is ~2x the covered-window figure (section 3: 1.9–2.5%), confirming the
  bias described in section 5, but lower than the 7.2% the settings preview
  showed. The preview runs inside a foreground app window and is not a
  substitute for a full-screen visible measurement.
* **GPU 7.2%**, inside the original < 8% GPU budget. The VideoDecode engine
  shows ~3%, independent confirmation that hardware decoding is in use.
* **The daemon held 2.4–2.6 MB and 0.00% CPU through every state.**
* Every transition happened as designed and was logged with its reason.
  The renderer restarted cleanly on the second reveal.
* Limitations: visible windows of 10 s and 22 s are short; one file, one
  GPU. Visible 60 fps and NVIDIA runs are still to do.

**Bug found by this session (fixed):** the frozen last frame could not be
set as the wallpaper (`SetWallpaper: file not found`), although the file
existed. Cause: the processes were launched from inside a packaged (MSIX)
app and inherited its file-system virtualisation. Their `AppData` writes
were redirected to `AppData\Local\Packages\<package>\LocalCache\`, visible to
them but not to Explorer, which is what actually opens wallpaper files.
Launching wallSet normally would not hit this, but shipping wallSet as an
MSIX package would. Fix: on "file not found" for a file that exists,
`set_static_wallpaper` retries from a copy in `%TEMP%`, which is not
redirected. Verified on the exact failing frame.

### 7. The settings window

| | Private | Working set |
|---|---|---|
| `wallset-ui` open, preview running | 272 MB median, 439 MB peak | 244 MB |
| `wallset-ui` closed | **0** | **0** |

eframe's OpenGL context pays the same GPU-driver device floor as the
renderer. That is acceptable only because the window is not resident: it
exits when closed, and closing it also terminates the preview renderer
(verified: no `wallset-*` processes left). A native Win32 or WinUI settings
window would avoid the OpenGL context entirely; recorded as a possible
improvement, not a priority.

## Budget revision

The budget in ARCHITECTURE.md section 1 was set before any measurement.

| State | Original target | Measured (best config) | Revised target |
|---|---|---|---|
| Idle, still image | < 10 MB, 0% | 2.2 MB host, 0% | unchanged |
| T0 playing, 1080p 24 fps | < 110 MB, < 2.5% CPU, < 8% GPU | **visible, full screen (section 6): 256–261 MB, 4.53–4.61% CPU, 7.2% GPU** on the iGPU; 340 MB on the dGPU (CPU not yet measured visible) | **< 400 MB, < 5% CPU, < 8% GPU**: met on the iGPU |
| T1 paused | 0% CPU | 0.00% | unchanged |
| T2 suspended | 0 MB, 0% | 0 MB, 0% | unchanged |

The original T0 figure assumed the renderer's memory would be dominated by
decoded frames. It is dominated by the GPU driver's per-process cost, which
no configuration removes. The architectural claim that matters, that the
renderer costs **nothing** whenever it cannot be seen, is met exactly.

## Measurement bug found and fixed

The first CPU figures read 1.54, 3.08, 4.62% — multiples of ~1.54. Windows
advances process CPU time in 15.625 ms clock ticks, so a 1-second sample can
only read multiples of 1.5625%. The harness was taking the median of
1-second samples, so its resolution equalled the effect size being measured.
Both `wallset measure` and `tools/bench.py` now compute CPU from the
CPU-time delta over the whole window (0.16% resolution over 10 s). Per-second
samples are kept only for spike detection. Section 2's CPU column predates
the fix and is marked as such.

## Open questions

1. **Power.** The iGPU result reduces CPU and private memory, but battery
   impact is unmeasured. Plan: 30-minute discharge test on battery per
   configuration, screen brightness fixed, same file.
2. **Intel 4K H.264 outlier (7.8% CPU).** Unverified hypothesis: the Intel
   decoder rejected that `-preset ultrafast` stream and mpv fell back to
   software. Check `hwdec-current` under `d3d11-adapter=Intel`.
3. **GPU utilisation** of the renderer, via the "GPU Engine" counters
   (`tools/bench.py` already reads them).
4. **Head-to-head with Lively Wallpaper** on this machine (it is installed),
   same file, same states, measured with `tools/bench.py --match lively`.
5. **Second machine.** Every number here is from one laptop. AMD and
   desktop NVIDIA will differ, especially the driver baseline.
6. **Visible CPU beyond the one measured case.** Section 6 measured
   1080p 24 fps on the iGPU only, in windows of 10 s and 22 s. Still needed,
   visible and full screen: 60 fps, 4K, and the NVIDIA GPU, at 30 s+ per run.

## Reproducing

```powershell
# test media
ffmpeg -f lavfi -i "testsrc2=size=1920x1080:rate=24:duration=10" -c:v libx264 -pix_fmt yuv420p -crf 23 good_1080p24_h264.mp4
ffmpeg -f lavfi -i "testsrc2=size=3840x2160:rate=24:duration=10" -c:v libx264 -preset ultrafast -crf 26 -pix_fmt yuv420p sdr_4k24_h264.mp4
ffmpeg -f lavfi -i "testsrc2=size=3840x2160:rate=60:duration=6" -c:v libx265 -preset ultrafast -crf 28 -pix_fmt yuv420p10le -tag:v hvc1 -x265-params "hdr10=1:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1):max-cll=1000,400" hdr_4k60_hevc10.mp4

# measurements
$env:WALLSET_LIBMPV = "$PWD\vendor\mpv\libmpv-2.dll"
.\target\release\wallset.exe measure good_1080p24_h264.mp4 --seconds 10
.\target\release\wallset.exe measure good_1080p24_h264.mp4 --seconds 10 --mpv-opt d3d11-adapter=Intel
.\target\release\wallset.exe demo good_1080p24_h264.mp4
```

Note for HDR test files: pass the colour tags through `-x265-params`.
FFmpeg's `-color_trc`/`-color_primaries` flags were silently dropped by the
libx265 wrapper here, which produces an untagged file that players correctly
treat as SDR.
