# Changelog

This file feeds the GitHub Release notes. Keep entries user-facing: describe what
changed for someone *using* MKVM, not the internal/CI plumbing. The release
workflow publishes whatever is under `## [Unreleased]`, so move those entries
under a version heading when you cut a release (or just leave them — the next
release will reuse them).

## [Unreleased]

### Fixed

- Linux server never forwarded keyboard input to the remote machine. `discover_mouse_devices` only grabbed mice (devices with `REL_X`/`REL_Y` + `BTN_LEFT`), so keyboard events went straight to the local compositor and never reached the edge-crossing/forwarding path. Added `discover_keyboard_devices` that finds devices with `KEY_SPACE`/`KEY_ENTER` (excluding mice so combo-dongle HID-mouse interfaces are not double-grabbed), and grabs them alongside mice in the same capture loop. `handle_evdev_event`'s existing `Key` branch already forwards through `handle_key` (remote when controlling, uinput reemit otherwise), so the local keyboard keeps working when no peer is active.
- Windows cursor was invisible while receiving remote mouse input. `inject_mouse_move` uses `SendInput`, which moves the cursor but does not make it visible if the desktop's `ShowCursor` counter is below zero — that happens when a previous MKVM server session (or any other app) called `ShowCursor(FALSE)` and exited without restoring, leaving the counter stuck negative. Added a one-shot `ensure_cursor_visible` that pumps `ShowCursor(TRUE)` until the counter reaches zero on the first injected mouse move of each process. Users see clicks/hover land but no arrow pointer otherwise.
- Local mouse cursor froze a few seconds after MKVM started on Linux, even with no peer connected. evdev's `fetch_events()` does a blocking `read()` unless the device fd is `O_NONBLOCK`, and MKVM grabs every mouse-like device (including keyboard HID mouse interfaces) — the capture thread stalled on the first device with no queued events, so accumulated mouse motion from other devices never reached `reemit_relative`. Grabbed devices are now opened non-blocking via `fcntl(F_SETFL, O_NONBLOCK)` so `fetch_events` returns `WouldBlock` when the kernel ring is empty and the loop keeps cycling.
- The Linux AppImage crashed at startup on Wayland compositors like niri with `Could not create surfaceless EGL display: EGL_BAD_ALLOC`. linuxdeploy bundles WebKitGTK/wayland/cairo without bundling Mesa, so the bundled WebKitGTK linked against the host's Mesa at runtime and hit an ABI mismatch in the GPU process. The release workflow now strips bundled libs that have same-SONAME host equivalents (`scripts/fix-appimage.sh`), so the AppImage prefers system libraries and only falls back to its bundle for libs the host doesn't ship.
- Keyboard, mouse, and clipboard could fail to connect between machines — the QUIC handshake rejected the peer with `invalid peer certificate: BadSignature`. The transport now pins the device's advertised certificate directly instead of running brittle chain validation over a self-signed certificate, which fixes cross-platform (macOS ↔ Windows) handshakes.

## v0.4.0

### Added

- Update indicator in the title bar: a download icon appears next to "MKVM" when a newer version is available — click it to open the update panel.

### Fixed

- "Latest version" in Settings now shows the latest released version once a check completes, instead of staying blank when you are already up to date.
- Corrected the clipboard sync description: images are synced too; only file clipboards are unsupported.

## v0.3.4

### Added

- Encrypted QUIC transport for keyboard, mouse, and clipboard traffic (TLS 1.3, pinned to the paired device's certificate).
- In-app updates: check GitHub Releases and install the latest version without leaving MKVM.
- Clipboard image sync — copy a picture on one machine and paste it on the other (text was already supported).
- Roam across a remote machine's multiple monitors.
- Cross-platform installers for macOS, Windows, and Linux, built automatically on each release.
- Signed macOS builds, so the Accessibility permission survives app updates.

### Improved

- Smoother, more seamless mouse hand-off when crossing between machines and displays.
- Better modifier-key remapping between macOS and Windows.
- Smoother slide-back when MKVM is not the front window on macOS.
- More reliable LAN discovery and manual peer connection.

### Fixed

- Trackpad two-finger scrolling on the Settings page.
- Faster, more reliable Windows clipboard sync.

## v0.1.0

- Added server/client onboarding and display layout editing.
- Added LAN discovery, manual peer connection, and shared input transport.
- Added text clipboard sync.
- Added English and Simplified Chinese UI strings.
- Added light, dark, and system theme modes.
- Added configurable single-port UDP transport with fallback.
- Added opt-in app performance monitoring.
- Added GitHub Actions CI and tag-based desktop release builds.
