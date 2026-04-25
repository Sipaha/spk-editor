# SPK Editor

SPK Editor is a personal fork of [Zed](https://zed.dev) by Zed Industries, Inc., modified by **Simonov Pavel** ([@Sipaha](https://github.com/Sipaha)).

This fork is built around tight integration with [Claude Code](https://claude.ai/code) as an external agent (via the Agent Client Protocol). It does **not** operate any of the Zed Industries cloud services that the upstream editor uses by default:

- No telemetry is sent.
- No auto-update channel — the binary is built from source.
- No Zed account / sign-in.
- No collab / channels / chat / voice.
- No Sentry crash uploads (panics are still logged locally).
- No native Zed cloud LLM provider — AI features go through the external `claude` subprocess.
- The Zed extension registry on `zed.dev` **is** still used for browsing and installing extensions.

## Building from source

Same toolchain requirements as upstream Zed (recent stable Rust, OS-specific dependencies — see upstream's README for the current list). After cloning:

```sh
cargo build --release
```

The binary lands at `target/release/zed` (the cargo crate name is unchanged for upstream-merge friendliness — copy or symlink it to `spk-editor` after building).

Bundling helpers per platform:

```sh
script/bundle-linux         # produces a tarball
script/bundle-mac           # produces SPK Editor.app
script/bundle-windows.ps1   # produces the Inno Setup installer
```

## Running unsigned binaries

SPK Editor binaries are **not signed or notarized**. To run on each OS:

- **Linux**: no extra step.
- **macOS**: Gatekeeper will refuse to launch. Right-click the app → Open, or run `xattr -dr com.apple.quarantine /Applications/SPK\ Editor.app`.
- **Windows**: SmartScreen will warn. Click "More info" → "Run anyway".

If you want signing, set up your own certificates and wire them through `script/bundle-mac` / `script/bundle-windows.ps1` (see `SPK_EDITOR_SIGN` env var).

## Icon

The shipped icon is currently the upstream Zed icon. To replace with placeholder spk-editor artwork, run (requires ImageMagick):

```sh
script/generate-placeholder-icons.sh
```

A proper icon design can replace the placeholder afterwards.

## Issues

Bug reports, feature requests, and questions: <https://github.com/Sipaha/spk-editor/issues>.

For upstream Zed bugs (anything not specific to this fork), please file directly at <https://github.com/zed-industries/zed>.

## License

SPK Editor inherits Zed's licensing unchanged:

- The editor (`crates/zed`) is licensed under **GPL-3.0-or-later**.
- The collab server (`crates/collab*`) is licensed under **AGPL-3.0** (kept in the tree but not built / run by default in spk-editor).
- The shared libraries (`gpui`, etc.) are licensed under **Apache-2.0**.

See `LICENSE-GPL`, `LICENSE-AGPL`, `LICENSE-APACHE`. All `Copyright Zed Industries, Inc.` notices are preserved per GPL §5(a). The legal documents inherited from upstream Zed are in `legal/upstream-zed/`; they describe Zed Industries' hosted services and **do not apply to spk-editor builds** (which operate no service infrastructure).

License-compliance for third-party dependencies is enforced by `cargo-about` (see `script/licenses/`). To re-check locally:

```sh
cargo install cargo-about
cargo about generate -c script/licenses/zed-licenses.toml templates/about.hbs > /dev/null
```

## Upstream

This fork is periodically merged from <https://github.com/zed-industries/zed>. Internal identifiers (cargo crate `zed`, modules, types) are kept unchanged from upstream to minimize merge friction; only user-visible identity (binary name, app bundle id, URL scheme, config directories, About dialog) is rebranded.

## Acknowledgements

All credit for the editor itself goes to **Zed Industries, Inc.** and the upstream Zed contributors. SPK Editor is a thin reskin + service-detachment layer on top of their work.
