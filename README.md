# MKVM

> **Forked from [XxMinor/mykvm](https://github.com/Genuineh/mkvm)** — this project adds Linux (Wayland/niri) input capture and injection support on top of the original mykvm, and is released as an independent project. Full credit for the original architecture (QUIC transport, edge-switching, clipboard sync, macOS/Windows backends) goes to the [original authors](https://github.com/Genuineh/mkvm).

**One keyboard, one mouse, one clipboard — shared across your Mac, Windows, and Linux machines on the same LAN.**

Move your cursor off the edge of one screen and it lands on the next machine. Your keyboard follows, and the clipboard (text and images) syncs automatically. No KVM hardware, no cables.

[![Download](https://img.shields.io/github/v/release/Genuineh/mkvm?label=Download&style=for-the-badge)](https://github.com/Genuineh/mkvm/releases/latest)
[![Stars](https://img.shields.io/github/stars/Genuineh/mkvm?label=Stars&logo=github&style=for-the-badge)](https://github.com/Genuineh/mkvm/stargazers)
[![Forks](https://img.shields.io/github/forks/Genuineh/mkvm?label=Forks&logo=github&style=for-the-badge)](https://github.com/Genuineh/mkvm/forks)
[![Platforms](https://img.shields.io/badge/platforms-macOS%20%7C%20Windows%20%7C%20Linux-2786ff?style=for-the-badge)](https://github.com/Genuineh/mkvm/releases/latest)
[![License: MIT](https://img.shields.io/badge/license-MIT-green?style=for-the-badge)](./LICENSE)

[中文说明](./README.zh-CN.md)

![MKVM tour](docs/screenshots/tour.gif)

## Screenshots

| Display layout | Devices | Settings |
| --- | --- | --- |
| ![Layout](docs/screenshots/layout.png) | ![Devices](docs/screenshots/devices.png) | ![Settings](docs/screenshots/settings.png) |

## Quick Start

1. **Install on both machines.** Download the installer for each OS from the [latest release](https://github.com/Genuineh/mkvm/releases/latest).
2. **Pick roles.** On the machine whose keyboard and mouse you want to share, open MKVM and keep **Server** mode (the default). On the other machine, open MKVM and switch to **Client** mode in Settings.
3. **Connect.** On the same LAN the two find each other automatically. Otherwise open **Devices**, type the other machine's IP (optionally `IP:port`), and click **Add**. Only devices that report their screen info join the layout.
4. **Arrange screens.** Open **Layout** and drag the monitors so their touching edges match how they sit on your desk.
5. **Cross over.** Push the cursor past a shared edge — it moves to the other machine. The keyboard follows, and copy/paste works in both directions.

## Permissions

- **macOS (server).** Grant MKVM both **Accessibility** and **Input Monitoring** under System Settings → Privacy & Security. These are required to capture and inject keyboard/mouse input. Signed builds keep the grant across updates; if it ever drops, toggle it off and on.
- **macOS first launch.** Builds are free self-signed (not Apple-notarized), so Gatekeeper warns the first time. Right-click the app → **Open** → **Open** to allow it once.
- **Windows.** No special permission for normal use. Run as Administrator only if you need to control elevated/admin windows.
- **Linux.** If you use the AppImage, mark it executable (`chmod +x`). For input capture/injection, see the **Linux (Wayland) Support** section below — you need `/dev/uinput` write access and `input` group membership.

## Linux (Wayland) Support

> This is the main feature MKVM adds over the original mykvm, which had no Linux input capture (edge switching was macOS/Windows only).

MKVM implements Linux input capture and injection at the **kernel level** using [`evdev`](https://docs.rs/evdev) and `uinput`. This is display-server agnostic: it works under **Wayland compositors like niri** that block display-server-level input capture (CGEventTap / X11 XRecord equivalents are unavailable or restricted under Wayland).

### How it works

- **Capture (server side):** Opens mouse `/dev/input/event*` nodes and calls `EVIOCGRAB` to exclusively grab them — the kernel stops delivering events to the compositor, so the local cursor freezes while we read relative `REL_X`/`REL_Y` deltas. A virtual cursor position is accumulated against the local screen bounds and fed into the same edge-crossing logic as the macOS/Windows backends (so edge-switching behavior is identical across platforms). When not controlling a remote, consumed relative events are re-emitted through a uinput virtual mouse so the local pointer still moves.
- **Injection (client side):** Received remote events are written to a uinput virtual keyboard/mouse. Windows virtual-key codes are mapped to/from evdev `KeyCode`s.

### Setup

```bash
# 1. Add your user to the input group (read access to /dev/input/event*)
sudo usermod -aG input $USER

# 2. Grant /dev/uinput write access via a udev rule (avoids running as root)
sudo tee /etc/udev/rules.d/99-mkvm-uinput.rules <<'EOF'
KERNEL=="uinput", SUBSYSTEM=="misc", MODE="0660", GROUP="input"
EOF
sudo udevadm control --reload-rules
sudo udevadm trigger /dev/uinput

# 3. Re-login (or `newgrp input`) for the group change to take effect
```

Verify:
- `ls -l /dev/uinput` shows `crw-rw---- ... input ...`
- `lsmod | grep uinput` shows the module loaded (if not: `sudo modprobe uinput`)

### Supported compositors

Because capture/injection happen at the kernel evdev/uinput layer, MKVM works under any Linux session: **niri, Sway, Hyprland, GNOME (Wayland), KDE (Wayland), and X11**. No compositor-specific protocol support is required.

### Known limitations

- **Multiple mice:** All detected mouse devices are grabbed and their relative deltas accumulated together. In practice only one mouse moves at a time, so this is rarely an issue.
- **Absolute cursor positioning (injection):** uinput mice are relative devices, so injected mouse moves use deltas from the last injected position. Wayland's security model prevents direct absolute cursor coordinate access; however, the server sends relative-move semantics, so behavior is correct in practice.
- **Local cursor hiding at the edge:** Unlike macOS, there is no reliable way to hide the local cursor while grabbing under Wayland. The grabbed cursor stays parked at the edge (frozen, not moving) during a crossing — visually acceptable but not as seamless as macOS.
- **Wire protocol compatibility:** MKVM retains the original `mykvm.*.v1` protocol markers, so it is wire-compatible with the original mykvm. A MKVM server can control a mykvm client and vice versa.

## Limitations

- **Trusted LAN only.** There is no user pairing/PIN yet, and LAN discovery is plaintext and unauthenticated. Do not expose the ports to public or untrusted networks.
- Input and clipboard ride an **encrypted QUIC/TLS** connection pinned to the peer's advertised certificate, but MKVM is a prototype and is not hardened for hostile networks.
- The clipboard syncs **text and images**, not files.
- macOS builds are **self-signed, not notarized** — expect a Gatekeeper prompt on first open.
- Experimental software: the protocol and behavior may change between versions.

---

## Features

- Runs in Server or Client mode.
- Discovers nearby peers on the LAN.
- Supports manual peer connection by host or IP.
- Detects local displays and lets you arrange multi-monitor layouts.
- Shares keyboard and mouse input over an encrypted QUIC connection.
- Syncs clipboard text and images over the same encrypted connection.
- Provides light, dark, and system theme modes.
- Includes English and Simplified Chinese UI.
- Supports tray behavior for hiding and restoring the main window.
- Checks GitHub Releases and updates itself in place.

## Current Status

MKVM is an experimental early release. It is useful for local testing and iteration, but it is not hardened for untrusted networks. See the [Releases page](https://github.com/Genuineh/mkvm/releases) for the current version and installers.

- License: MIT
- Default ports: UDP `47833` (discovery) and UDP `47834` (QUIC transport)
- Clipboard payload caps: 256 KB text, 32 MB image
- Transport security: input and clipboard run over a TLS 1.3 (QUIC) connection pinned to the peer's advertised certificate
- Security model: trusted LAN prototype
- Not yet included: user pairing/PIN, authenticated discovery, and production transport hardening

Do not expose the transport ports to public or untrusted networks.

## Protocol

MKVM runs two channels. LAN discovery uses a plain UDP port; input and clipboard run over an encrypted QUIC connection on a second UDP port.

| Channel | Default port | Transport | Marker | Purpose |
| --- | --- | --- | --- | --- |
| Discovery | UDP `47833` | UDP datagrams | `mykvm.discovery.v1` | LAN discovery, peer probe/reply, host info, and display metadata |
| Input | UDP `47834` | QUIC datagrams | `mykvm.input.v1` | Mouse movement, mouse buttons, scroll, and keyboard events (low latency, loss tolerant) |
| Clipboard | UDP `47834` | QUIC streams | `mykvm.clipboard.v1` | Clipboard text and image sync (reliable, ordered) |

The discovery port is configurable in Settings (default UDP `47833`); the QUIC transport port defaults to the discovery port + 1 (UDP `47834`). Both auto-fall-back through nearby ports if a port is taken, and can use a system-selected port if needed. Peers advertise their active discovery port, QUIC port, transport public key, and protocol version, so discovered and manually added devices connect to the right port and pin the right certificate.

The QUIC connection is TLS 1.3 encrypted: each peer generates a self-signed certificate at startup and advertises it during discovery, and the connecting side pins that certificate, so input and clipboard traffic is encrypted and bound to the advertised peer. Discovery itself is still plaintext and unauthenticated, so keep MKVM on a trusted LAN.

## Requirements

- Node.js 22+
- Rust stable
- Platform desktop toolchain:
  - Windows: Microsoft C++ Build Tools
  - macOS: Xcode Command Line Tools
  - Linux: WebKitGTK and appindicator development packages

## Development

Install dependencies:

```bash
npm install
```

Run the web UI:

```bash
npm run dev
```

Run the Tauri desktop app:

```bash
npm run tauri:dev
```

Build without bundling installers:

```bash
npm run tauri:build
```

Build desktop bundles:

```bash
npm run tauri:bundle
```

## Platform Helpers

Windows:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\check-dev-env.ps1
powershell -ExecutionPolicy Bypass -File .\scripts\run-tauri-dev.ps1
```

macOS and Linux:

```bash
sh scripts/check-dev-env.sh
sh scripts/run-tauri-dev.sh
```

macOS input capture and injection require Accessibility and Input Monitoring permissions in System Settings.

## Verification

Run these before opening a pull request or cutting a release:

```bash
npm run build
npm run lint
cargo check --manifest-path src-tauri/Cargo.toml
```

## Release

Git itself only stores and pushes source history. GitHub Actions does the actual packaging on GitHub-hosted runners.

The release workflow watches pushes to `main`:

- `feat:` publishes the next minor version, such as `v0.1.0` to `v0.2.0`.
- `fix:` publishes the next patch version, such as `v0.1.0` to `v0.1.1`.
- Other prefixes run normal checks but do not publish a release.
- If no release tag exists yet, the first `feat:` or `fix:` push publishes `v0.1.0`.

Release notes come from the `## [Unreleased]` section of [CHANGELOG.md](./CHANGELOG.md) (user-facing wording), falling back to filtered commit subjects. Keep that section up to date as you land changes.

Example:

```bash
git commit -m "feat: initial desktop release"
git push origin main
```

The workflow creates the git tag, builds macOS, Windows, and Linux bundles, then publishes a GitHub Release with the generated installers.

## Project Layout

| Path | Purpose |
| --- | --- |
| `src/App.tsx` | Main React desktop console |
| `src/desktopApi.ts` | Frontend bridge to Tauri commands |
| `src/layout.ts` | Display layout transforms and adjacency logic |
| `src/runtime.ts` | Runtime status types |
| `src-tauri/src/lib.rs` | Tauri commands, UDP discovery, clipboard sync, app state, and performance sampling |
| `src-tauri/src/input.rs` | Input capture, forwarding, and injection runtime |
| `src-tauri/src/quic_transport.rs` | Encrypted QUIC transport (input datagrams, clipboard streams) with certificate pinning |
| `scripts/` | Development and build helper scripts |

## Contributing

Issues and pull requests are welcome. Keep changes focused, document behavior that affects the protocol, and verify both the web build and the Tauri backend when touching shared runtime code.

See [CONTRIBUTING.md](./CONTRIBUTING.md) for commit prefixes and versioning notes.

## License

MIT. See [LICENSE](./LICENSE).
