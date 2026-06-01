# Mighty IDE UI Test Tools

These scripts support the Windows UX loop for Mighty IDE. They intentionally
exercise the packaged app through the real window, mouse, keyboard, trace file,
or screenshot paths instead of only calling Rust unit tests.

## Primary gates

- `overlay-gallery.ps1` renders deterministic PNGs for modal, overlay, and panel
  states using `MUI_*_AUTOOPEN` hooks.
- `win-ui-harness.ps1` drives the packaged EXE through mouse, keyboard, tab,
  file dialog, dirty-close, dock-resize, and rail-navigation flows.

## Focused probes

- `capture-window.ps1` launches the IDE, optionally posts simple input, and
  captures the window with `PrintWindow`.
- `drive-input.ps1` launches the IDE, drives real desktop input, and screenshots
  the visible window region.
- `live-input.ps1` uses `SendInput` directly for an interactive desktop session.
- `cap-test.ps1` is a minimal screen-capture smoke check for the host session.

The focused probes are useful when diagnosing DPI, foreground-window, or GPU
capture behavior. The main release confidence gate should still be
`win-ui-harness.ps1` plus targeted gallery captures.
