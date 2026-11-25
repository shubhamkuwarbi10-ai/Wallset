# Design

The settings window (`crates/wallset-ui`) is wallpaper-led: the wallpaper is
the room. That is a product commitment (see PRODUCT.md), chosen by the user to
replace the earlier Windows 11 Settings look.

## World

- **Backdrop:** the chosen file, else what the selected display shows now, else
  the Windows wallpaper. Its Windows thumbnail (the same one Explorer shows,
  so no decoder ships) is shrunk to 96 px wide, box-blurred, darkened (as if
  under black at alpha 165) and stretched over the whole window. The title bar
  is tinted to the backdrop's top edge. A file Windows cannot thumbnail gets a
  plain dark backdrop.
- **Black floor:** the darkened backdrop never goes below 40/255. Dark, smoothly
  varying areas came out corrupted on the development machine's compositor
  (OpenGL and Direct3D alike); a raised floor avoids it. Do not remove it
  without re-testing on that kind of machine.
- **Colour:** always dark. Panels are glass: near-black at about 60% opacity
  with a white hairline. One accent, taken from the wallpaper (the dominant
  hue, made light enough to carry black text), falling back to the Windows
  accent. The accent marks selection, the primary action, toggles that are on
  and slider rails.
- **Type and icons:** Segoe UI and Segoe UI Semibold with Segoe Fluent Icons
  (MDL2 on Windows 10), loaded from `%WINDIR%\Fonts`, never bundled. Ramp:
  Caption 12, Body 14, Subtitle 20, Title 28.
- **Surfaces:** panels 12 px radius (dock, details), cards 8 px, controls 4 px.
  Text over pictures sits on a dark pill.

## Structure

- Header: centred page switcher (Wallpaper / Display / Performance), then an
  InfoBar for the last result or error.
- **Wallpaper** (default): a large preview at the display's aspect ratio
  showing the picture cropped exactly as the chosen fit mode will crop it, with
  the display's name and an "On your desktop now" / "Not applied yet" chip.
  Below it a dock: display tiles to scale (when there are several), Fill / Fit
  / Stretch, Choose file, Preview, Apply. Below the dock one summary line: the
  file, its cost, its fit, its format, and "Details", which opens a panel with
  the cost (estimated, then measured during preview) and the fit check.
- **History:** every wallpaper applied before, newest first, as cards with a
  thumbnail. Each says whether the file is still on this PC ("Missing: moved or
  deleted" otherwise), when it was used, and offers Use (or Download again
  for a missing file that came from a link). The pencil icon opens the path,
  type, fit, last change and source link, with Show in folder, the Windows
  Properties dialog, and Remove.
- **From link:** a dock button opens a link field in place of the summary
  line. The file is downloaded once (Windows' own curl.exe) into
  `%LOCALAPPDATA%\wallSet\downloads`, then treated like a chosen file.
- **Display:** the capability report as a settings list, gamut meters, and the
  best files for this display, in a centred column.
- **Performance:** status, then toggles and sliders in settings rows. Each
  change saves at once and writes only its own section.

## Constraints

- The preview is a native child window. Nothing may open over its rectangle:
  no drop-downs, combo boxes or tooltips over it. Choices are segmented
  controls.
- Switching pages or displays stops the preview.
- Repaint stays reactive. The backdrop loads on a worker thread that wakes the
  window once. An idle window uses 0 CPU (measured: 0 ms CPU over 5 s).
