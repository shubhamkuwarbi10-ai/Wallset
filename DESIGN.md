# Design

The settings window (`crates/wallset-ui`) follows Windows 11. That is a product
commitment (see PRODUCT.md), not a style choice to revisit casually.

## World

- **Type:** Segoe UI and Segoe UI Semibold, with Segoe Fluent Icons (MDL2 on
  Windows 10), all loaded from `%WINDIR%\Fonts` at runtime and never bundled.
  WinUI ramp: Caption 12, Body 14, Subtitle 20 semibold, Title 28 semibold.
- **Colour:** WinUI neutral tokens for light and dark (`theme.rs`), following the
  Windows app theme. The one accent is the user's own Windows accent, read from
  `AccentPalette` (Light 2 on dark, Dark 1 on light), falling back to Windows blue.
  The accent marks only selection, the primary action, toggles that are on, and slider rails.
- **Surfaces:** a flat base, cards at 8 px radius with a 1 px stroke, controls at
  4 px. No shadows, no blur, no gradients.

## Structure

- Header: page title on the left, a SelectorBar (Wallpaper / Display /
  Performance) on the right, then an InfoBar for the last result or error.
- **Wallpaper** (default): the displays drawn to scale in their real arrangement
  (click to choose one), then a preview at the display's aspect ratio, then a
  command bar (Fill / Fit / Stretch, Preview, Apply). A side column holds the
  file, its cost while visible (estimated, then measured during preview), and
  the fit check.
- **Display:** the capability report as a settings list, gamut meters, and the
  best files for this display.
- **Performance:** status, then toggles and sliders in Windows Settings rows.
  Each change saves at once and writes only its own section.

## Constraints

- The preview is a native child window. Nothing may open over its rectangle: no
  drop-downs, combo boxes or tooltips on the Wallpaper page's left column.
  Choices there are segmented controls.
- Switching pages or displays stops the preview.
- Repaint stays reactive. An idle window uses 0 CPU (measured: 0 ms CPU over 5 s).
