# Patched winit 0.30.13

The workspace patches crates.io winit to `vendor/winit` so every ordinary Cargo
build includes the Windows pending-event fix. This is the complete published
0.30.13 crate, excluding Cargo's local `.cargo-ok` cache marker, with only
`src/platform_impl/windows/event_loop.rs` changed. The upstream license and VCS
metadata are retained.

The patch prevents accepted user events from becoming permanently stranded when
Windows rejects a wake notification. Focus reporting and the NVDA provider remain
enabled. The vendored crate is excluded from the Alacritty workspace so upstream
examples, tests and formatting do not become Alacritty workspace targets.

`winit-files.sha256.json` records the exact crate files. The reviewable patch,
upstream/patched backend hashes, incident evidence and Windows RED/GREEN verifier
are in `docs/investigations/windows-user-event-queue/`.

When upgrading, reproduce the failed baseline, port or remove the patch only
after equivalent delivery proof, regenerate the file hashes and Cargo.lock, and
run the Windows gate plus real terminal focus/close checks. Do not edit the Cargo
registry or require command-line dependency overrides for a release build.
