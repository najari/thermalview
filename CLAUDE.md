# ThermalView — agent notes

Read `docs/DEVELOPMENT.md` before changing code; it is the source of truth for scope, structure and the hardware rules (section 4). Update it first when a design decision changes.

- Build: `cargo build --release` (MSVC toolchain, Rust in `%USERPROFILE%\.cargo\bin`). Check before committing: `cargo fmt`, `cargo clippy --all-targets`, `cargo test`.
- `unsafe` only in `crates/libuvc-sys`, `src/camera/uvc.rs`, `src/video/mf.rs` and `src/webcam/mf.rs` (Media Foundation).
- Do not revert the claim-before-probe patch in `crates/libuvc-sys/vendor/libuvc` (see its `PATCHES.md`).
- The camera is opened exclusively (WinUSB): close GetThermal or other instances before hardware tests.
- The Lepton has no shutter: never send FFC without the user confirming the lens is covered.
