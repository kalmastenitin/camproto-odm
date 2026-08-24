# camproto-odm
 
A cross-platform **ONVIF Device Manager**, built in Rust + [egui](https://github.com/emilk/egui).
Discover IP cameras and NVRs on your network, inspect and configure them, and
view live and recorded video — all from a single native app on macOS and
Windows (Linux discovery/config works today; Linux video decode isn't wired
up yet).
 
[![CI](https://github.com/kalmastenitin/camproto-odm/actions/workflows/ci.yaml/badge.svg)](https://github.com/kalmastenitin/camproto-odm/actions/workflows/ci.yaml)
[![Security audit](https://github.com/kalmastenitin/camproto-odm/actions/workflows/audit.yml/badge.svg)](https://github.com/kalmastenitin/camproto-odm/actions/workflows/audit.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
 
## Why
 
Most ONVIF tooling is Windows-only, closed source, and struggles to render
modern H.265 streams. camproto-odm aims to be:
 
- **Cross-platform** — one codebase, native binaries for macOS and Windows today.
- **Dependency-light** — pure Rust. No OpenSSL, no C crypto toolchain
  (`ring`/`aws-lc`), so the build is identical everywhere. This is enforced in
  CI via [`deny.toml`](deny.toml).
- **First-principles** — WS-Discovery, SOAP, and WS-Security are hand-rolled, not
  wrapped, so behavior is legible and portable.

## Status
 
Early, and built in the open, one increment at a time.
 
| Feature | ONVIF service | Status |
|---|---|---|
| Discovery (multicast + targeted IP/subnet/CIDR) | WS-Discovery / device service | ✅ done |
| Connect + device info | WS-Security + `GetDeviceInformation` | ✅ done |
| Media profiles + RTSP URL | `GetProfiles` / `GetStreamUri` | ✅ done |
| Live video (hardware-accelerated decode) | RTSP → decode → egui texture | ✅ done on macOS (VideoToolbox) and Windows (FFmpeg, D3D11VA hw decode with software fallback). Not yet wired on Linux |
| Snapshot capture (JPEG) | `GetSnapshotUri` + HTTP fetch | ✅ done |
| Playback (recorded video) | Profile G — `GetRecordings` / replay | ✅ done |
| PTZ (pan/tilt/zoom + presets) | PTZ service | ✅ done |
| Imaging controls (exposure, WB, focus, etc.) | Imaging service | ✅ done — read *and* write |
| Date/time, network, NTP, hostname, reboot | Device service | ✅ done |
| Event alerts (PullPoint) | Events service | ✅ done |
| Event snapshot thumbnails | Events + `GetSnapshotUri` | 🚧 in progress — alerts arrive, but the per-event thumbnail isn't wired to the alert yet |
| User management | Device service | ⏳ planned |
| Linux live video decode | RTSP → decode | ⏳ planned |
 
## Download
 
Prebuilt binaries for macOS (universal: Intel + Apple Silicon) and Windows
(x64) are published on the [Releases page](https://github.com/kalmastenitin/camproto-odm/releases).
Each is a plain zip containing the executable + license — there's no installer.

These builds are **unsigned** (no Apple notarization, no Windows code-signing
certificate yet):

- **macOS**: Gatekeeper will refuse to open it normally. Right-click the
  binary → **Open** → **Open** again in the confirmation dialog, or run
  `xattr -d com.apple.quarantine camproto-odm` first. You'll also need to
  `chmod +x camproto-odm`.
- **Windows**: SmartScreen may warn "Windows protected your PC" on first run —
  click **More info** → **Run anyway**. The Windows build links FFmpeg
  statically, so no separate FFmpeg DLLs are needed; it does still expect the
  Microsoft Visual C++ runtime, which is already present on essentially all
  Windows 10/11 machines.

## Build & run
 
Requires Rust (see [`rust-toolchain.toml`](rust-toolchain.toml)).

camproto-odm depends on [`camproto-ingest`](https://github.com/kalmastenitin/camproto-ingest)
via a relative path (`../camproto-ingest`), so clone it as a sibling directory
before building:

```bash
git clone https://github.com/kalmastenitin/camproto-ingest ../camproto-ingest
cargo run --release
```
 
**Linux** additionally needs the windowing dev headers:
 
```bash
sudo apt-get install -y \
  libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev \
  libxkbcommon-dev libgl1-mesa-dev
```
 
macOS need no extra system packages.

### Building on Windows

The Windows video decoder is backed by FFmpeg (libavcodec) via `ffmpeg-sys-next`,
so FFmpeg development libraries must be present at build time. The simplest route
is vcpkg — the same setup camproto-nvr uses:

    git clone https://github.com/microsoft/vcpkg
    .\vcpkg\bootstrap-vcpkg.bat
    .\vcpkg\vcpkg install ffmpeg:x64-windows-static-md

Then point the build at your vcpkg install so `ffmpeg-sys-next` can find it:

    setx VCPKG_ROOT C:\path\to\vcpkg

(Restart the shell after `setx`.) The `-static-md` triplet links FFmpeg
statically against the dynamic MSVC CRT, which avoids shipping FFmpeg DLLs.
If you prefer, `ffmpeg:x64-windows` (dynamic) works too.

Alternatively, if you already have a prebuilt FFmpeg (e.g. a BtbN or gyan.dev
build), skip vcpkg and set `FFMPEG_DIR` to its root instead.

> Hardware decode (D3D11VA) is used automatically when the installed FFmpeg
> supports it; otherwise the decoder falls back to software. No config needed.

macOS needs no extra prerequisites (VideoToolbox). Linux video decode is not
yet wired up.

## Usage
 
- **Discover LAN** — multicast WS-Discovery across your subnet.
- **Target + Scan** — probe a specific `IP`, `IP:port`, range (`.10-50`), or CIDR
  (`192.168.1.0/24`). Reaches other subnets that multicast can't.
- Select a device to see its identity and service endpoints; enter shared
  credentials and **Connect** to authenticate.
- **Live** — view the camera's stream (hardware-decoded on macOS/Windows),
  with a PTZ overlay (pan/tilt/zoom, presets) and live imaging controls
  (exposure, white balance, focus, etc.) for cameras that support them.
- **Playback** — browse SD-card/NVR recordings (Profile G) and scrub a
  timeline to replay them.
- **Events** — subscribe to PullPoint analytics alerts and watch them stream
  in live.
- **Settings** — read and set device date/time, NTP, network, and hostname,
  and reboot the device.

## Security
 
- Credentials are held **in memory only** — never logged, never written to disk.
- Authentication uses WS-Security UsernameToken digest
  (`Base64(SHA1(nonce + created + password))`); the password is never sent in
  clear and never appears in logs.
- Supply chain is gated by `cargo-deny` (advisories, licenses, banned crates,
  source allow-list) on every PR and weekly.
## Contributing
 
Issues and PRs welcome. CI runs `fmt`, `clippy`, `test`, and a release build on
all three OSes, plus the security audit — please make sure those pass locally:
 
```bash
cargo fmt --all
cargo clippy --all-targets
cargo test
cargo deny check   # requires: cargo install cargo-deny
```

Maintainers: the [Release workflow](.github/workflows/release.yml) is manual
(`workflow_dispatch`, under the Actions tab) — it builds the Windows and
macOS-universal binaries and publishes them to a GitHub Release under the
given tag (or the version in `Cargo.toml` if left blank).
 
## License
 
MIT — see [LICENSE](LICENSE).