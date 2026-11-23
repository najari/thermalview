# Local patches to winit 0.30.13

Source: the published `winit` 0.30.13 crate (Apache-2.0, see `LICENSE`). It is used through
`[patch.crates-io]` in the workspace `Cargo.toml`.

## 1. Mixed-DPI move on Windows 11

Symptom: dragging the window to a monitor with a different scale makes it grow repeatedly and
bounce back, so it cannot cross.

Cause: the `WM_DPICHANGED` handler replaces the rectangle suggested by Windows with its own. It
validates that rectangle with `MonitorFromWindow`, which can still name the monitor being left
during a move. The window is nudged back, DPI flips again, and the size is rescaled again.

Change, in `src/platform_impl/windows/`:

- `event_loop.rs`: on Windows 11 (build 22000 or later), a window that is neither maximized nor
  fullscreen is placed at the rectangle Windows suggests.
- `dark_mode.rs`: `WIN10_BUILD_VERSION` is visible to `event_loop.rs`.

Windows 10, maximized windows and fullscreen windows keep the original code path.

Verified on 2026-11-24 on Windows 11 build 26300 with monitors at 100% and 150%: the user dragged
the app window between them and it crossed normally.

Upstream: fixed on winit master by rust-windowing/winit#4341 (0.31 betas). The 0.30 backport,
rust-windowing/winit#4694, was still open on 2026-11-24.

Remove this crate and the `[patch.crates-io]` entry once eframe depends on a winit with the fix.
