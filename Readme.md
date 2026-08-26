# camproto-odm
 
A cross-platform **ONVIF Device Manager**, built in Rust + [egui](https://github.com/emilk/egui).
Discover IP cameras and NVRs on your network, inspect them, and (soon) view live
and recorded video — all from a single native app on macOS, Windows, and Linux.
 
[![CI](https://github.com/kalmastenitin/camproto-odm/actions/workflows/ci.yml/badge.svg)](https://github.com/kalmastenitin/camproto-odm/actions/workflows/ci.yml)
[![Security audit](https://github.com/kalmastenitin/camproto-odm/actions/workflows/audit.yml/badge.svg)](https://github.com/kalmastenitin/camproto-odm/actions/workflows/audit.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

<p align="center">
  <img src="assets/screenshot.png" alt="camproto-odm ONVIF Device Manager" width="800">
  <br>
  <em>ONVIF Device Manager — live view with PTZ overlay and timeline seek</em>
</p>

</p>

## Why
 
Most ONVIF tooling is Windows-only, closed source, and struggles to render
modern H.265 streams. camproto-odm aims to be:
 
- **Cross-platform** — one codebase, native binaries for all three desktop OSes.
- **Dependency-light** — pure Rust. No OpenSSL, no C crypto toolchain
  (`ring`/`aws-lc`), so the build is identical everywhere. This is enforced in
  CI via [`deny.toml`](deny.toml).
- **First-principles** — WS-Discovery, SOAP, and WS-Security are hand-rolled, not
  wrapped, so behavior is legible and portable.
## Status
 
Early, and built in the open, one increment at a time.
 
| Feature | ONVIF service | Status |
|---|---|---|
| Discovery (multicast + targeted IP/subnet) | WS-Discovery / device service | ✅ done |
| Connect + device info | WS-Security + `GetDeviceInformation` | 🚧 in progress |
| Media profiles + RTSP URL | `GetProfiles` / `GetStreamUri` | ⏳ next |
| Live video (hardware-accelerated decode) | RTSP → decode → wgpu | ⏳ planned |
| Playback (SD-card recordings) | Profile G replay | ⏳ planned |
| Event snapshots (analytics alerts) | Events / PullPoint | ⏳ planned |
| PTZ, imaging, users, time | various | ⏳ planned |
 
## Build & run
 
Requires Rust (see [`rust-toolchain.toml`](rust-toolchain.toml)).
 
```bash
cargo run --release
```
 
**Linux** additionally needs the windowing dev headers:
 
```bash
sudo apt-get install -y \
  libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev \
  libxkbcommon-dev libgl1-mesa-dev
```
 
macOS and Windows need no extra system packages.
 
## Usage
 
- **Discover LAN** — multicast WS-Discovery across your subnet.
- **Target + Scan** — probe a specific `IP`, `IP:port`, range (`.10-50`), or CIDR
  (`192.168.1.0/24`). Reaches other subnets that multicast can't.
- Select a device to see its identity and service endpoints; enter shared
  credentials and **Connect** to authenticate.
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
 
## License
 
MIT — see [LICENSE](LICENSE).