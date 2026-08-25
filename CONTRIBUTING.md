# Contributing to camproto-odm

Thanks for the interest — this project is early and built in the open, so
issues and PRs of any size (typos to features) are welcome.

## Getting set up

1. Requires Rust — see [`rust-toolchain.toml`](rust-toolchain.toml) for the
   pinned channel; `rustup` will pick it up automatically.
2. camproto-odm depends on [`camproto-ingest`](https://github.com/kalmastenitin/camproto-ingest)
   via a relative path (`../camproto-ingest`), so clone it as a sibling
   directory:
   ```bash
   git clone https://github.com/kalmastenitin/camproto-ingest ../camproto-ingest
   ```
3. Platform-specific build prerequisites (Linux windowing headers, Windows
   FFmpeg/vcpkg setup) are documented in the [README's Build & run
   section](README.md#build--run) — follow those before `cargo run`.

## Before opening a PR

CI runs `fmt`, `clippy`, `test`, and a release build on Linux, macOS, and
Windows, plus `cargo-deny` for the security audit. Please make sure all of
these pass locally first — it's much faster to catch issues here than to
wait on CI:

```bash
cargo fmt --all
cargo clippy --all-targets --all-features
cargo test --all-features
cargo deny check   # requires: cargo install cargo-deny
```

If you're touching platform-specific code (`src/video/decode/`), and you
only have one OS to test on, say so in the PR description — CI will still
exercise the others.

## Opening a PR

1. Fork the repo (or branch directly if you have write access) and make your
   changes on a feature branch.
2. Open a PR against `main`. The PR template will prompt you for a summary
   and a checklist of what you've verified locally.
3. `main` is protected: a PR is required, and all CI checks (the three OS
   builds + `cargo-deny`) must pass before it can merge.
4. Keep PRs focused — a bug fix doesn't need an unrelated refactor riding
   along with it; it's easier to review and easier to revert if something
   goes wrong.

## Code style

- No unnecessary abstractions or speculative flexibility — this codebase
  favors a few duplicated lines over a premature helper.
- Comments explain *why*, not *what* — skip comments that just restate what
  well-named code already says.
- Keep dependencies light. This project deliberately avoids OpenSSL/C crypto
  toolchains (`ring`/`aws-lc` are fine, `openssl`/`native-tls` are not) so
  the build stays identical across platforms — this is enforced by
  [`deny.toml`](deny.toml), not just convention.
- Credentials are held in memory only — never log them, never write them to
  disk, and never include them (even redacted) in test fixtures or issue
  reports.

## License

By contributing, you agree your contributions are licensed under this
project's [MIT license](LICENSE).
