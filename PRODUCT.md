# Product

<!-- impeccable:product-schema 1 -->

## Platform

windows-desktop (native Win32 + egui; macOS/Linux are stubbed for later)

## Users
Both everyday Windows users who just want an image or video as their wallpaper, and tech-savvy users who want to see display specs and measured resource cost. The simple path (choose, preview, apply) comes first; the technical report is one step away, never in the way.

## Product Purpose
Set any image or video as the desktop wallpaper, per monitor, with a live preview first, while staying nearly free in the background. Success: the wallpaper looks right on this specific display and costs ~0% CPU and ~0 MB when not visible.

## Positioning
Lively Wallpaper and Wallpaper Engine already do video wallpapers. wallSet's difference is measured, not claimed: it shows the real CPU and memory a wallpaper costs on this PC before you apply it, checks the file against the display's actual capabilities (resolution, refresh, HDR, gamut), and drops to 0% CPU when covered and 0 MB after a grace period.

## Operating Context
A settings window opened occasionally from the tray or Start menu, used for a minute, then closed (it exits on close, so it costs nothing while closed). A background service does the playback. Typical hardware: laptops with an integrated plus a discrete GPU, high-refresh panels, 125% scaling.

## Capabilities and Constraints
- Per-monitor assignment; fit modes Fill (crop), Fit (bars), Stretch.
- The preview is a native child window placed over a fixed rectangle. Nothing in the UI (menus, combo popups, tooltips) may open over that rectangle while previewing.
- Repaint is reactive; an idle settings window must use no CPU.
- Fitness findings carry severities: Problem, Warning, Advice, Info, with optional ffmpeg fix commands.
- Resource settings: GPU decode, low-power scaling, power-saving GPU, efficiency mode, pause when covered, grace seconds, battery rules, start with Windows.

## Brand Commitments
- Wallpaper-led (user choice, replacing the earlier "Windows 11 Settings" look): the wallpaper itself is the window's backdrop and the preview is the centrepiece; controls sit on it as quiet translucent panels. Still a Windows app underneath: Segoe UI, Fluent icons, Windows conventions. Not a gaming or neon look.
- Name is written "wallSet".

## Evidence on Hand
Measured numbers in docs/BENCHMARKS.md (reference laptop: daemon 2.4 MB, 0% CPU; 1080p24 visible ~4.6% CPU, ~258 MB). No testimonials, user counts or third-party comparisons exist; do not invent any.

## Product Principles
1. Show, then apply: nothing changes the desktop until the user has seen it.
2. Numbers are measured on this PC, and say so.
3. Simple first, depth on demand.
4. Costs nothing when you are not looking at it, the window included.
