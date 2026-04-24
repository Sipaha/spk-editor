# SPK Editor — Rebrand of Zed (Design Spec)

- **Date:** 2026-04-25
- **Author:** Simonov Pavel (`Sipaha`)
- **Status:** Draft, pending user approval
- **Upstream:** `zed-industries/zed` @ commit at the time of spec writing (`e2b02e2290` on `main`)

---

## 1. Goals

Create a personal fork of Zed branded as **SPK Editor**, used as a daily-driver editor with tight integration to Claude Code (subprocess via ACP). The fork must be detached from Zed Industries cloud services from day one, but able to keep receiving upstream changes from `zed-industries/zed` via periodic merges.

### In scope

1. **User-visible rebrand** to `SPK Editor` / `spk-editor` / `ru.sipaha.spk-editor` on Linux, macOS, and Windows: app bundle, binary, URL scheme, config / state / cache directories, window titles, About dialog, CLI help, icons (placeholder), README.
2. **Disable Zed Industries-controlled services**: collab, auto-update, telemetry, edit-prediction (Zeta), Zed sign-in, Zed cloud LLM proxy + native agent threads, Sentry crash reporting, feedback emails to Zed.
3. **Keep working features** that do not depend on Zed-Industries-controlled servers:
   - Extension registry on `zed.dev` (browse / install extensions).
   - Agent panel **only** for external agents via ACP (Claude Code subprocess).
4. **License compliance**: preserve all existing `Copyright Zed Industries, Inc.` notices, add modified-version marking per GPL §5(a), add attribution in About dialog and README.

### Non-goals (explicit)

- **Renaming internal identifiers** (cargo crate `zed`, `pub mod zed`, `enum ReleaseChannel::*` variants, type names) — kept as-is to minimize merge friction with upstream.
- **Own infrastructure**: no own auto-update server, no own telemetry / Sentry, no own LLM proxy, no own collab server, no own documentation site.
- **Final icon design** — placeholder only; user will design proper icons later.
- **Settings migration** from `~/.config/zed/` to `~/.config/spk-editor/` — start clean.
- **Replacing the extension registry** — keep using `zed.dev`'s registry as-is.
- **Code signing / notarization** — binaries unsigned. README will document how to allow them on each OS.
- **CI publishing to GitHub Releases** — local builds only for now. Existing `.github/workflows/` workflows that depend on Zed-internal secrets will be disabled (`if: false` or trigger removal), not "fixed for our setup".
- **Rewriting the docs site** under `docs/` — not touched. Links to `https://zed.dev/docs` remain (point to upstream).
- **Replacing legal documents** with custom Terms / Privacy Policy — moved to `legal/upstream-zed/` and marked as upstream-inherited, not applicable to spk-editor builds.
- **FreeBSD packaging** — not in our three target platforms; `script/bundle-freebsd` left untouched.

---

## 2. Approach

**Layered surgical edits**: replace user-visible string literals at the points where they are already concentrated (`crates/paths/src/paths.rs`, `crates/release_channel/src/lib.rs`, plist / manifest / installer files, scattered runtime strings via grep). No new abstractions introduced — no central `branding` crate, no codegen — to keep the diff against upstream as small as possible.

This consciously trades "easy to rebrand again" for "easy to merge from upstream", because the second is a recurring cost and the first is a one-time job.

### Why not centralize via constants

A `crates/branding/` constants module would touch many more Rust files (every site that currently writes `"Zed"` would gain a `use branding::APP_DISPLAY_NAME` import), increasing merge-conflict surface. Static configs (`.plist`, `.iss`, `Cargo.toml`) cannot reference Rust constants anyway, so the "single source of truth" promise would not be fully delivered.

### Why not build-time codegen

Build-script that templates plist / manifest from a `branding.toml` is overkill for a one-off rebrand, makes debugging harder (paths come from `OUT_DIR`-generated files), and templates would break frequently as upstream evolves the underlying files.

---

## 3. Identifier Decisions (locked)

| Parameter | Value |
|---|---|
| Display name | `SPK Editor` |
| CLI binary | `spk-editor` |
| macOS bundle ID — stable | `ru.sipaha.spk-editor` |
| macOS bundle ID — dev | `ru.sipaha.spk-editor-dev` |
| macOS bundle ID — debug | `ru.sipaha.spk-editor-debug` |
| macOS bundle | `SPK Editor.app` |
| Windows install dir | `spk-editor` |
| Windows AppUserModelID | `Sipaha.SpkEditor` |
| URL scheme | `spk-editor://` |
| Config dir — Linux | `~/.config/spk-editor/` |
| Config dir — macOS | `~/Library/Application Support/spk-editor/` |
| Config dir — Windows | `%APPDATA%\spk-editor\` |
| State / log dir — Linux | `~/.local/share/spk-editor/`, `~/.local/state/spk-editor/` |
| Cache dir — Linux | `~/.cache/spk-editor/` |
| GitHub repo | `Sipaha/spk-editor` (`https://github.com/Sipaha/spk-editor`) |
| Issue tracker URL | `https://github.com/Sipaha/spk-editor/issues` |
| Icon | Placeholder; replaced later by user |
| Attribution string | `Fork of Zed by Zed Industries, Inc., modified by Simonov Pavel` |
| License | Unchanged (`GPL-3.0-or-later` for the editor crate; AGPL-3.0 for collab; Apache-2.0 for libraries) |
| Settings migration | None — start with empty config dir |

### Internal identifiers kept as-is (non-goal to rename)

`crates/zed/`, `crates/zed/src/zed.rs::pub mod zed`, `enum ReleaseChannel { Stable, Preview, Nightly, Dev }` (variant names unchanged; only `display_name()` / `app_id()` outputs change), all type names, all module paths.

---

## 4. File-Level Change Map

Concrete files / locations identified during exploration. Counts are approximate; final list to be enumerated by the implementation plan.

### 4.1 Identity & paths

| File | Change |
|---|---|
| `crates/paths/src/paths.rs` | Replace `"Zed"` / `"zed"` literals at lines 94, 101, 103, 121, 125, 136, 145, 150, 162, 168, 177, 180 with `"SPK Editor"` / `"spk-editor"` per OS (lines may shift by impl time). |
| `crates/release_channel/src/lib.rs` | `display_name()` (line 187 area): per-channel strings → `"SPK Editor"`, `"SPK Editor Preview"`, `"SPK Editor Nightly"`, `"SPK Editor Dev"`. `app_id()` and `app_identifier()` return `ru.sipaha.spk-editor[-suffix]`. Enum variants stay. |
| `crates/zed/Cargo.toml` | `name = "zed"` stays. Bundle metadata fields (product name, description) → `SPK Editor`. License field unchanged. |
| Workspace `Cargo.toml` | If it carries a workspace-level product name / description — update; otherwise leave. |

### 4.2 Platform configs and installers

| File | Change |
|---|---|
| `crates/zed/resources/zed.desktop.in` → `spk-editor.desktop.in` | Rename. Edit: `Name=SPK Editor`, `Exec=spk-editor`, `Icon=spk-editor`, `MimeType=x-scheme-handler/spk-editor;`, `StartupWMClass=spk-editor`. Update referencing build scripts. |
| `crates/zed/resources/zed.entitlements` → `spk-editor.entitlements` | Rename only (contents are entitlement keys, name-agnostic). |
| `crates/zed/resources/info/Info.plist` (or wherever the macOS plist lives — to be confirmed by impl plan) | `CFBundleName`, `CFBundleDisplayName`, `CFBundleIdentifier`, `CFBundleURLSchemes`, `CFBundleExecutable`. |
| `debug.plist` (repo root) | `ru.sipaha.spk-editor-debug` and matching display name. |
| `crates/zed/resources/windows/zed.iss` → `spk-editor.iss` | `[Setup]`: `AppName=SPK Editor`, `AppPublisher=Simonov Pavel`, `AppPublisherURL=https://github.com/Sipaha/spk-editor`, `AppId={{ru.sipaha.spk-editor}}`, `DefaultDirName={pf}\spk-editor`, `OutputBaseFilename=spk-editor-setup`. |
| `crates/zed/resources/windows/zed.sh` → `spk-editor.sh` | Rename. Update reference paths. |
| `crates/zed/resources/windows/sign.ps1` | Not modified (we do not sign). |
| `crates/zed/resources/windows/messages/*.isl` | Replace any human-visible "Zed" occurrences with "SPK Editor". |
| `crates/zed/resources/flatpak/*`, `crates/zed/resources/snap/*` | Update app id, display name, exec path in manifests if present. |
| Icons: `assets/icons/app/app-icon{,-dev,-preview,-nightly}{,@2x}.png`, `assets/icons/app/Document.icns`, `crates/zed/resources/windows/app-icon{,-dev,-preview,-nightly}.ico` | Replace with placeholder (single solid-color image with letter `S`); generate all required sizes via a small script (`script/generate-placeholder-icons.sh`) committed alongside. README marks as TODO. |
| `assets/images/zed_logo.svg`, `assets/images/zed_x_copilot.svg` | Replace with placeholder SVG. |
| `assets/icons/zed_*.svg` (zed_predict, zed_assistant, zed_agent, etc.) | These are referenced by Zeta / native agent UI. Since those features are disabled, keep files as-is (no need to rename — they're internal asset paths, never user-visible). |

### 4.3 Build scripts

| File | Change |
|---|---|
| `script/bundle-linux` | `zed.desktop.in` → `spk-editor.desktop.in`; output artifact names (`zed-linux-*.tar.gz` → `spk-editor-linux-*.tar.gz`); install paths. |
| `script/bundle-mac` | `Zed.app` → `SPK Editor.app`; `Zed-*.dmg` → `spk-editor-*.dmg`; entitlements/plist references. |
| `script/bundle-windows.ps1` | Output `zed-*.exe` → `spk-editor-*.exe`; `.iss` path. |
| `script/install.sh`, `script/uninstall.sh` | `zed` → `spk-editor` in install paths, binary name, `.desktop` filename. |
| `script/bundle-freebsd` | Untouched (out of scope). |

### 4.4 Runtime user-visible strings (Rust)

| Category | Files (representative) | Change |
|---|---|---|
| About dialog | `crates/zed/src/zed.rs` (action handler for About / OpenAbout) | Text: `SPK Editor v{version} ({commit_sha})\nFork of Zed by Zed Industries, Inc., modified by Simonov Pavel.\nSource: https://github.com/Sipaha/spk-editor` |
| Window titles & app name displays | `crates/workspace/`, `crates/title_bar/`, sites that read `display_name()` | Already routed via `ReleaseChannel::display_name()` — no edits needed beyond §4.1. Audit for hardcoded `"Zed"` string literals not going through `display_name()`. |
| CLI help / argparse | `crates/cli/`, `crates/zed/src/main.rs` | clap `name = "spk-editor"`, `about = "SPK Editor — fork of Zed"`. |
| URLs in code — feedback | `crates/feedback/src/feedback.rs` | `ZED_REPO_URL` → `https://github.com/Sipaha/spk-editor`; `REQUEST_FEATURE_URL` → `…/discussions/new/choose`; file-issue URL → `…/issues/new`; `mailto:hi@zed.dev` → removed (or → file-issue URL). |
| URLs in code — `zed_urls` module | `crates/client/src/zed_urls.rs` | Functions for **extension registry** URLs stay pointing at `zed.dev` (we keep using it). Functions for `account`, `start_trial`, `upgrade` — left intact (called only by sign-in UI which we hide; they will simply never fire). |
| Welcome / onboarding | `crates/onboarding/`, `crates/ai_onboarding/`, `crates/welcome/` | Remove "Sign in to Zed" and Zed Pro upsell sections; replace remaining `"Zed"` strings with `"SPK Editor"`; keep generic onboarding (open project, configure keymap). |

### 4.5 Service disablement

All disablements are done **without removing crates** — they remain in the workspace for merge-friendliness.

| Service | Disablement point | Method |
|---|---|---|
| `auto_update` | `crates/zed/src/zed.rs` (init site) | Skip `auto_update::init`. Auto-updater UI hidden. |
| `telemetry` | `crates/client/src/telemetry.rs` (or equivalent) and default settings | Hard-code "do not send" at the dispatch layer (defense in depth — even if a setting is flipped, no events leave the process). Default settings: `telemetry: { metrics: false, diagnostics: false }`. |
| `collab` / `collab_ui` | `crates/zed/src/zed.rs` init site; UI registration of contacts / channels / chat panels | Skip `collab_ui::init` (or equivalent); panels not registered. |
| Sign-in / Zed account | `crates/title_bar/`, sites that show "Sign in to Zed" | Hide UI affordances. Keep `client::authenticate` code path (unused). |
| `cloud_llm_client` + native agent threads | `crates/agent/`, `crates/language_models/src/provider/cloud.rs`, `crates/agent_ui/` | In agent panel, register only ACP-thread provider (external agents). Hide native Zed thread provider from UI selectors. |
| `zeta` (edit prediction) | `crates/zed/src/zed.rs` init site; `crates/edit_prediction_ui/` | Skip Zeta init; default `edit_predictions.provider: "none"`. |
| `feedback` | See §4.4 — URL replacement, no disablement (button works, opens our GitHub issues). | |
| Sentry crash report | `crates/zed/src/reliability.rs` | Locate the DSN / endpoint constant (line ~270+ uses sentry tagging — confirm during impl). Disable form upload (return early). Keep local panic logging to disk. |

### 4.6 License & attribution

| File | Change |
|---|---|
| `LICENSE-GPL`, `LICENSE-AGPL`, `LICENSE-APACHE` | **Untouched.** Zed Industries copyright preserved. |
| `README.md` | Full rewrite. New title `# SPK Editor`. About section: `SPK Editor is a personal fork of [Zed](https://zed.dev) by Zed Industries, Inc., modified by Simonov Pavel. Distributed under the same licenses as upstream Zed (GPL-3.0-or-later for the editor; AGPL-3.0 for collab; Apache-2.0 for libraries).` Remove Sponsorship / Hiring sections. Keep Licensing section (cargo-about CI). Add: build-from-source instructions, "this is a fork" disclaimer, link to upstream, TODO marker for icons, instructions for Linux/macOS/Windows users on running unsigned binaries. |
| `CONTRIBUTING.md` | Replace with short stub: this is a personal fork; PRs are welcome at `Sipaha/spk-editor` but are evaluated case-by-case; upstream contributions should be sent to `zed-industries/zed` directly. |
| `CODE_OF_CONDUCT.md` | Untouched (inherited from upstream). |
| `legal/{terms,privacy-policy,subprocessors,third-party-terms}.md` | Move to `legal/upstream-zed/`. Add `legal/README.md` explaining: "These documents apply to Zed Industries' hosted services. spk-editor is a personal fork that does not operate any service infrastructure (no telemetry, no auto-update server, no collab server, no LLM proxy). They are preserved here for license-attribution completeness." |
| About dialog text | See §4.4. |
| `crates/zed/Cargo.toml` `license` field | Unchanged. |

---

## 5. Build / CI Impact

### Local builds

- `cargo build` / `./script/clippy` continue to work — internal crate name `zed` preserved.
- Bundling scripts (`script/bundle-linux`, `script/bundle-mac`, `script/bundle-windows.ps1`) updated to produce `spk-editor`-named artifacts.
- New helper script `script/generate-placeholder-icons.sh` to (re)generate placeholder icons in all required sizes / formats.

### CI

- **No new workflows added.**
- Existing workflows in `.github/workflows/` that depend on Zed-internal secrets (codesign certs, notarization API keys, Zed-controlled S3 buckets, Cloudflare tokens, Vercel deploys, Sentry release upload) are disabled by either:
  - removing the trigger (`on: …` block emptied or branch filter removed), or
  - prepending `if: false` to all jobs.
- The choice between the two is left to the implementation plan based on whether the workflow is "useful but blocked on secrets" (keep but `if: false`, easy to revive) or "definitely not for us" (remove triggers).
- `cargo-about` license-check workflow (used to gate license compliance) — keep enabled if it does not require Zed-only secrets.

### Distribution

- Out of scope for this rebrand. Users build from source. README documents this.

---

## 6. License Compliance Checklist

To be verified after implementation:

- [ ] All `LICENSE-*` files unchanged byte-for-byte vs upstream at fork point.
- [ ] No `Copyright Zed Industries, Inc.` lines removed from any file.
- [ ] Attribution line `Fork of Zed by Zed Industries, Inc., modified by Simonov Pavel` present in:
  - [ ] About dialog (visible at runtime).
  - [ ] `README.md`.
- [ ] `crates/zed/Cargo.toml` `license` field unchanged.
- [ ] `legal/upstream-zed/` retains the original Zed legal documents with a clear "applies to upstream Zed services, not this fork" note.
- [ ] No relicensing of any file: where Zed assigns `Apache-2.0` or `GPL-3.0-or-later` or `AGPL-3.0`, the same SPDX identifier remains in our tree.
- [ ] If any new file is added by this rebrand and it incorporates substantial upstream-derived code, it carries the upstream license; new files that are purely our own (e.g., a brand-new icon-generation script) may be licensed by us under one of the project's existing licenses.

---

## 7. Verification Plan

After implementation, the following checks must pass before merging the rebrand commit / branch:

1. **Build**: `cargo build --release` succeeds on Linux. (macOS / Windows builds verified opportunistically — user is on Linux primary.)
2. **Lints**: `./script/clippy` passes.
3. **Run smoke test on Linux**:
   - Binary launches, window title shows `SPK Editor`.
   - About dialog shows correct attribution line.
   - First launch creates `~/.config/spk-editor/`, `~/.local/share/spk-editor/`, `~/.cache/spk-editor/` (and **no** `~/.config/zed/`).
   - Settings UI / keymap UI work.
   - Extension panel loads list from `zed.dev` (registry still works).
   - Agent panel: only external (ACP / Claude Code) provider available; no "Sign in to Zed" or native cloud thread option.
   - Click "Give feedback" / equivalent → opens `https://github.com/Sipaha/spk-editor/issues` (not `zed-industries/zed`).
   - No "Sign in" / Zed account UI present in title bar or onboarding.
   - Open a `.rs` file: edit prediction does not appear / Zeta is silent.
4. **Network audit** (manual, with `tcpdump` or `mitmproxy` against the running binary):
   - No connections to `api.zed.dev`, `collab.zed.dev`, `telemetry.zed.dev`, `cloud.zed.dev`, or any Sentry endpoint.
   - Connections to `zed.dev` are **only** for the extension registry (e.g., `zed.dev/api/extensions`).
   - Connections to LLM providers happen only via Claude Code subprocess (i.e., editor itself does not contact `api.anthropic.com`).
5. **Auto-update**: confirm no auto-update HTTP request is fired at launch and no auto-update UI banner appears.
6. **Crash report**: trigger a panic in a test build; confirm panic is logged to disk only and no Sentry POST happens.
7. **License audit**: run `cargo about generate` (or `script/licenses/...`) and confirm it still passes.
8. **Upstream merge dry-run**: `git merge upstream/main --no-commit --no-ff` against a recent upstream `main`; record the conflict count and which files conflict. (Goal: <30 files, all in surface areas we touched intentionally.)

---

## 8. Open Questions / Deferred

- **Final icon design**: placeholder ships now; user replaces later. Tracked in README.
- **Sentry DSN location**: exact location of the Sentry endpoint constant in `crates/zed/src/reliability.rs` will be pinned down during implementation (the grep earlier showed sentry-form-tag construction but not the destination URL). Disablement is done at the upload site regardless.
- **Welcome screen content**: deferred to implementation — the exact list of screens / steps to keep vs. drop will be decided when looking at `crates/welcome/` and `crates/onboarding/` directly.
- **`.app` bundle structure on macOS**: exact `Info.plist` location will be pinned down during implementation (it may be generated rather than checked in).
- **macOS / Windows verification**: user is on Linux; macOS / Windows builds will be sanity-checked by reading the diffs but not runtime-verified in this session. Tracked as follow-up when user has access to those platforms.
- **Custom auto-update channel** (point at our own GitHub Releases): explicitly deferred. Decision when / if we start publishing binaries.
- **Custom telemetry**: not planned. If ever needed, a future spec.
- **CI-signed Linux packages** (deb / rpm / AUR / Flatpak / Snap): deferred. Local source build is the only supported install path for now.

---

## 9. Implementation Order (preview — full plan to be produced by writing-plans skill)

1. Identifier foundations: `crates/paths`, `crates/release_channel`, `Cargo.toml` metadata.
2. Static platform configs: `.desktop`, `.entitlements`, `.plist`, `.iss`, `messages/*.isl`, flatpak / snap manifests.
3. Bundling scripts: `script/bundle-linux`, `bundle-mac`, `bundle-windows.ps1`, `install.sh`, `uninstall.sh`.
4. Placeholder icons: generation script + committed assets.
5. Service disablement: `auto_update`, `telemetry`, `collab` / `collab_ui`, sign-in UI, `cloud_llm_client` / native agent threads, `zeta`, Sentry upload, `feedback` URLs.
6. Runtime user-visible strings: About, CLI help, welcome / onboarding cleanup.
7. License & attribution: `README.md`, `CONTRIBUTING.md`, `legal/` reorganization.
8. CI cleanup: disable workflows depending on Zed-internal secrets.
9. Verification (manual smoke + network audit + license audit + upstream merge dry-run).

---

## 10. Risks

- **Hidden Zed-cloud calls**: there may be code paths we did not identify that contact `*.zed.dev`. Network audit (Verification §4) is the catch-net. If something is found, fix at the call site.
- **Upstream merge friction**: even with surgical edits, future upstream changes to `crates/paths` / `crates/release_channel` / `crates/feedback` will conflict. Acceptable cost; manageable per merge.
- **Extension registry dependency**: spk-editor's "extensions work" property is hostage to Zed Industries continuing to operate `zed.dev/api/extensions` openly (no auth required for browse / install). If they ever lock it, extensions break. No mitigation in this spec — flagged as a known coupling.
- **Sentry DSN confusion**: if the Sentry DSN is configured via env var rather than a code constant, our disablement at the upload site is the correct guard. Verified by Verification §6.
- **Icon placeholder shipped to users**: if any user installs spk-editor before the user replaces icons, they see a generic placeholder. Acceptable for personal-fork posture.
