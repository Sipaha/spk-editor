# SPK Editor Rebrand — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rebrand the Zed fork to SPK Editor — user-visible identity (`SPK Editor` / `spk-editor` / `ru.sipaha.spk-editor`) on Linux/macOS/Windows, disable Zed-Industries-controlled services (collab, auto-update, telemetry, Zeta, sign-in, cloud LLM proxy, Sentry, feedback emails), keep extension registry on `zed.dev` and ACP-based external agents (Claude Code subprocess) working.

**Architecture:** Layered surgical edits. Internal identifiers (`crates/zed`, `pub mod zed`, `enum ReleaseChannel`) stay unchanged to minimize merge friction with upstream. Only user-visible string literals at concentrated points (`crates/paths`, `crates/release_channel`, plist / installer / scripts, scattered runtime strings) are replaced. Services are disabled at init / dispatch sites; their crates remain in the workspace.

**Tech Stack:** Rust workspace (Cargo), GPUI, Inno Setup (Windows installer), `.desktop` (Linux), `Info.plist` (macOS), bash / PowerShell bundling scripts, `cargo-about` for license check.

**Spec:** `docs/superpowers/specs/2026-04-25-spk-editor-rebrand-design.md`

---

## Overview of phases

- **Phase A** — Identity foundations: `paths`, `release_channel`. Has unit tests.
- **Phase B** — Static platform configs: `.desktop`, `.entitlements`, `Info.plist`, `.iss`, `.isl`, flatpak / snap. Verified by inspection + build.
- **Phase C** — Build scripts: `bundle-{linux,mac,windows.ps1}`, `install.sh`, `uninstall.sh`. Verified by inspection.
- **Phase D** — Placeholder icons.
- **Phase E** — Service disablement: `auto_update`, `telemetry`, `collab`/`collab_ui`, sign-in, native cloud LLM, `zeta`, Sentry, `feedback` URLs.
- **Phase F** — Runtime user-visible strings: About, CLI help, welcome / onboarding, residual `"Zed"` literals.
- **Phase G** — License & attribution: `README.md`, `CONTRIBUTING.md`, `legal/`.
- **Phase H** — CI cleanup: disable workflows depending on Zed-internal secrets.
- **Phase I** — Verification: build, smoke, network audit, license audit, upstream merge dry-run.

Total: 38 tasks. Each task ends with a commit.

**Commit-message convention (per project CLAUDE.md):**
- No `Co-Authored-By` line.
- Imperative mood, capitalized first word, no trailing period, no `feat:` / `fix:` prefixes.
- Optional crate prefix when one crate is the clear scope (`paths: …`, `release_channel: …`).

---

## Task 1: Create rebrand branch

**Files:** none (git operation only).

- [ ] **Step 1: Verify clean working tree**

Run: `git status --short`
Expected: only the unrelated `M codebook.toml` left over from before the spec commit; no other uncommitted changes. If anything else is uncommitted, stop and ask the user.

- [ ] **Step 2: Stash unrelated changes**

Run: `git stash push -m "pre-rebrand stash" -- codebook.toml`
Expected: stash created.

- [ ] **Step 3: Create branch**

Run: `git checkout -b rebrand/spk-editor`
Expected: switched to a new branch `rebrand/spk-editor`.

- [ ] **Step 4: Confirm**

Run: `git status && git branch --show-current`
Expected: clean working tree on branch `rebrand/spk-editor`.

---

# Phase A — Identity foundations

## Task 2: Rebrand `crates/paths`

**Files:**
- Modify: `crates/paths/src/paths.rs` (replace `"Zed"` / `"zed"` literals at lines 94, 101, 103, 121, 125, 136, 145, 150, 162, 168, 177, 180 — line numbers may have shifted; locate by content).
- Test: `crates/paths/src/paths.rs` (add inline `#[cfg(test)] mod tests` if absent, otherwise extend).

- [ ] **Step 1: Read the file to locate every `"Zed"` / `"zed"` string**

Run: `grep -n '"Zed"\|"zed"' crates/paths/src/paths.rs`
Expected: ~12 hits across `home_dir/Library/Application Support`, `~/.config`, `~/.local/share`, `~/.local/state`, `~/.cache`, `%APPDATA%` builders. Note each line.

- [ ] **Step 2: Write a failing test asserting new directory names**

Add to bottom of `crates/paths/src/paths.rs`:

```rust
#[cfg(test)]
mod rebrand_tests {
    use super::*;

    #[test]
    fn config_dir_contains_spk_editor() {
        let p = config_dir();
        assert!(
            p.to_string_lossy().contains("spk-editor") || p.to_string_lossy().contains("SPK Editor"),
            "config_dir should mention spk-editor; got {p:?}"
        );
        assert!(
            !p.to_string_lossy().to_ascii_lowercase().contains("zed"),
            "config_dir must not mention zed; got {p:?}"
        );
    }

    #[test]
    fn data_dir_contains_spk_editor() {
        let p = data_dir();
        assert!(
            p.to_string_lossy().contains("spk-editor") || p.to_string_lossy().contains("SPK Editor"),
            "data_dir should mention spk-editor; got {p:?}"
        );
        assert!(
            !p.to_string_lossy().to_ascii_lowercase().contains("zed"),
            "data_dir must not mention zed; got {p:?}"
        );
    }
}
```

(If `data_dir` does not exist, substitute the actual public function name discovered in Step 1 — e.g. `support_dir`, `state_dir`. Add one test per directory function exposed by the crate.)

- [ ] **Step 3: Run tests to verify failure**

Run: `cargo test -p paths rebrand_tests -- --nocapture`
Expected: FAIL with `config_dir must not mention zed`.

- [ ] **Step 4: Replace literals**

For each hit from Step 1:
- `"Zed"` → `"SPK Editor"`
- `"zed"` → `"spk-editor"`

Use `Edit` per occurrence (or `replace_all` after confirming uniqueness). Do not change anything else in the file. Internal identifiers in this file are limited to the literal directory names; nothing else needs renaming.

- [ ] **Step 5: Run tests to verify pass**

Run: `cargo test -p paths -- --nocapture`
Expected: all tests in the `paths` crate PASS, including the two new ones.

- [ ] **Step 6: Run clippy**

Run: `./script/clippy -p paths`
Expected: no warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/paths/src/paths.rs
git commit -m "paths: Rebrand directory names to spk-editor"
```

---

## Task 3: Rebrand `crates/release_channel`

**Files:**
- Modify: `crates/release_channel/src/lib.rs` (`display_name()` ~line 187, `app_id()` / `app_identifier()` ~line 29 / 204 — locate by content).
- Test: same file.

- [ ] **Step 1: Inspect current values**

Run: `grep -n '"Zed"\|app_id\|app_identifier\|display_name\|dev_name' crates/release_channel/src/lib.rs`
Expected: lines for `display_name`, `dev_name`, `app_id`, `app_identifier` (the Stable variant returns `"Zed"`; others return `"Zed Preview"`, `"Zed Nightly"`, `"Zed Dev"`, and the app-id functions return `dev.zed.Zed*`).

- [ ] **Step 2: Write failing tests**

Add to the bottom of `crates/release_channel/src/lib.rs`:

```rust
#[cfg(test)]
mod rebrand_tests {
    use super::*;

    #[test]
    fn display_names_use_spk_editor() {
        assert_eq!(ReleaseChannel::Stable.display_name(), "SPK Editor");
        assert_eq!(ReleaseChannel::Preview.display_name(), "SPK Editor Preview");
        assert_eq!(ReleaseChannel::Nightly.display_name(), "SPK Editor Nightly");
        assert_eq!(ReleaseChannel::Dev.display_name(), "SPK Editor Dev");
    }

    #[test]
    fn app_ids_use_ru_sipaha() {
        for ch in [
            ReleaseChannel::Stable,
            ReleaseChannel::Preview,
            ReleaseChannel::Nightly,
            ReleaseChannel::Dev,
        ] {
            let id = ch.app_id();
            assert!(
                id.starts_with("ru.sipaha.spk-editor"),
                "app_id for {ch:?} should start with ru.sipaha.spk-editor; got {id}"
            );
        }
    }
}
```

(If `dev_name`, `app_id`, or `app_identifier` have different signatures or variant names than this template assumes, adapt the test to call them; use the actual signatures observed in Step 1. Do not invent variants — only test the four `ReleaseChannel` variants that exist.)

- [ ] **Step 3: Run tests to verify failure**

Run: `cargo test -p release_channel rebrand_tests -- --nocapture`
Expected: FAIL with mismatched strings.

- [ ] **Step 4: Update `display_name()`**

Replace per-variant literals in `display_name()`:
- `"Zed"` → `"SPK Editor"`
- `"Zed Preview"` → `"SPK Editor Preview"`
- `"Zed Nightly"` → `"SPK Editor Nightly"`
- `"Zed Dev"` → `"SPK Editor Dev"`

- [ ] **Step 5: Update `dev_name()` (if it returns name strings)**

Inspect `dev_name()`'s return values. If they look like `"Zed-Dev"` or `"Zed-Preview"`, update them to the `spk-editor` equivalents (`"spk-editor-dev"`, `"spk-editor-preview"` — matching the kebab-case convention used for the binary). If `dev_name()` instead returns something like a per-channel folder suffix (`"-dev"`, `"-preview"`), leave the suffix logic untouched.

- [ ] **Step 6: Update `app_id()` / `app_identifier()`**

Replace per-variant returns:
- Stable: `"dev.zed.Zed"` → `"ru.sipaha.spk-editor"`
- Preview: `"dev.zed.Zed-Preview"` → `"ru.sipaha.spk-editor-preview"`
- Nightly: `"dev.zed.Zed-Nightly"` → `"ru.sipaha.spk-editor-nightly"`
- Dev: `"dev.zed.Zed-Dev"` → `"ru.sipaha.spk-editor-dev"`

(Adapt to actual literals seen in Step 1; the spec only locks Stable / Dev / Debug ids — Preview / Nightly follow the same pattern.)

- [ ] **Step 7: Run tests to verify pass**

Run: `cargo test -p release_channel -- --nocapture`
Expected: all tests PASS.

- [ ] **Step 8: Run clippy**

Run: `./script/clippy -p release_channel`
Expected: no warnings.

- [ ] **Step 9: Commit**

```bash
git add crates/release_channel/src/lib.rs
git commit -m "release_channel: Rename channels to SPK Editor and ru.sipaha.spk-editor app ids"
```

---

# Phase B — Static platform configs

## Task 4: Rename and edit Linux `.desktop` file

**Files:**
- Rename: `crates/zed/resources/zed.desktop.in` → `crates/zed/resources/spk-editor.desktop.in`
- Modify: same file (after rename).

- [ ] **Step 1: Inspect current contents**

Run: `cat crates/zed/resources/zed.desktop.in`
Expected: a `.desktop` template with `Name=`, `Exec=`, `Icon=`, `MimeType=`, `StartupWMClass=` keys, possibly with `@APP_NAME@`-style placeholders.

- [ ] **Step 2: Rename the file**

Run: `git mv crates/zed/resources/zed.desktop.in crates/zed/resources/spk-editor.desktop.in`

- [ ] **Step 3: Edit the file**

Replace keys (use exact values; do not parameterize):
- `Name=Zed` → `Name=SPK Editor`
- `GenericName=` line keep as upstream (e.g. `GenericName=Text Editor`).
- `Exec=zed …` → `Exec=spk-editor …` (preserve any `%U` / `%F` arguments).
- `Icon=zed` → `Icon=spk-editor`
- `MimeType=` — replace any `x-scheme-handler/zed;` with `x-scheme-handler/spk-editor;`. Preserve other MIME entries unchanged.
- `StartupWMClass=zed` → `StartupWMClass=spk-editor` (if present).
- Any other `Zed` token in human-visible fields (`Comment=`, `Keywords=`) → `SPK Editor`.

If the file contains `@…@` placeholders that get substituted at bundle time, leave those placeholders intact and update the substitution in `script/bundle-linux` later (Task 13). The placeholders themselves are not user-visible.

- [ ] **Step 4: Verify**

Run: `cat crates/zed/resources/spk-editor.desktop.in`
Expected: `Name=SPK Editor`, `Exec=spk-editor …`, `Icon=spk-editor`, `MimeType=…x-scheme-handler/spk-editor;…`.

- [ ] **Step 5: Find external references to the old filename**

Run: `grep -rn 'zed\.desktop\.in\|zed\.desktop' --include='*.sh' --include='*.ps1' --include='*.rs' --include='*.toml' .`
Expected: list of files referencing the old name. Note them — they will be updated in their respective tasks (mainly `script/bundle-linux` in Task 13).

- [ ] **Step 6: Commit**

```bash
git add crates/zed/resources/spk-editor.desktop.in
git commit -m "Rename zed.desktop.in to spk-editor.desktop.in and rebrand keys"
```

---

## Task 5: Rename macOS `.entitlements` file

**Files:**
- Rename: `crates/zed/resources/zed.entitlements` → `crates/zed/resources/spk-editor.entitlements`

- [ ] **Step 1: Rename**

Run: `git mv crates/zed/resources/zed.entitlements crates/zed/resources/spk-editor.entitlements`

- [ ] **Step 2: Verify contents are name-agnostic**

Run: `cat crates/zed/resources/spk-editor.entitlements`
Expected: a plist of entitlement keys (e.g. `com.apple.security.cs.allow-jit`, `com.apple.security.cs.disable-library-validation`). No `Zed` literal inside. If a `Zed` literal appears (e.g. inside a comment), leave it for now and move on — it will be handled by Task 27 (residual literals).

- [ ] **Step 3: Find external references**

Run: `grep -rn 'zed\.entitlements' --include='*.sh' --include='*.ps1' --include='*.rs' .`
Expected: hits in `script/bundle-mac`. Recorded for Task 14.

- [ ] **Step 4: Commit**

```bash
git add crates/zed/resources/spk-editor.entitlements
git commit -m "Rename zed.entitlements to spk-editor.entitlements"
```

---

## Task 6: Edit macOS `Info.plist`

**Files:**
- Modify: macOS plist source(s) — exact path(s) to be discovered. Candidates: `crates/zed/resources/info/Info.plist`, `crates/zed/resources/macos/Info.plist`, or a template inside `script/bundle-mac`.

- [ ] **Step 1: Locate plist sources**

Run: `find crates/zed/resources -name 'Info.plist' -o -name 'Info.plist.in' -o -name 'Info-*.plist'`
Expected: one or more plist files. Also check `script/bundle-mac` for an inline `<<EOF` plist.

If no checked-in plist is found, run: `grep -n 'CFBundleIdentifier\|CFBundleName\|CFBundleDisplayName' script/bundle-mac` — the plist may be generated by the bundling script. In that case, this task edits `script/bundle-mac` (which is the same file Task 14 touches; merge this task into Task 14 if so, and note it in the commit).

- [ ] **Step 2: Edit each plist**

For each `Info.plist` found, replace:
- `<key>CFBundleName</key>` value → `SPK Editor`
- `<key>CFBundleDisplayName</key>` value → `SPK Editor`
- `<key>CFBundleIdentifier</key>` value → `ru.sipaha.spk-editor` (or the per-channel suffixed form if there are channel-specific plists: `ru.sipaha.spk-editor-preview`, `-nightly`, `-dev`)
- `<key>CFBundleExecutable</key>` value → `spk-editor`
- Inside `<key>CFBundleURLTypes</key>`: `<key>CFBundleURLSchemes</key>` array entry `zed` → `spk-editor`. The accompanying `<key>CFBundleURLName</key>` (if present) → `ru.sipaha.spk-editor.url`.
- `<key>CFBundleGetInfoString</key>` (if present) → `SPK Editor` (drop any embedded copyright; we keep Zed's elsewhere via attribution).
- `<key>NSHumanReadableCopyright</key>` (if present) — **keep upstream** (`Copyright © Zed Industries, Inc.`); do not modify (license-compliance).

- [ ] **Step 3: Verify**

For each edited plist, run: `plutil -lint <path>` if `plutil` is available (macOS-only). On Linux, run: `xmllint --noout <path>` instead.
Expected: file is valid XML / plist.

- [ ] **Step 4: Commit**

```bash
git add <edited paths>
git commit -m "Set macOS bundle ids and display name to SPK Editor"
```

---

## Task 7: Edit `debug.plist`

**Files:**
- Modify: `debug.plist` (repo root).

- [ ] **Step 1: Inspect**

Run: `cat debug.plist`
Expected: a small plist with debug-bundle keys.

- [ ] **Step 2: Edit**

Replace:
- `CFBundleIdentifier` value → `ru.sipaha.spk-editor-debug`
- `CFBundleName` / `CFBundleDisplayName` (if present) → `SPK Editor Debug`
- `CFBundleExecutable` (if present) → `spk-editor-debug`

- [ ] **Step 3: Verify**

Run: `xmllint --noout debug.plist`
Expected: valid XML.

- [ ] **Step 4: Commit**

```bash
git add debug.plist
git commit -m "Set debug bundle id to ru.sipaha.spk-editor-debug"
```

---

## Task 8: Rename and edit Windows Inno Setup script

**Files:**
- Rename: `crates/zed/resources/windows/zed.iss` → `crates/zed/resources/windows/spk-editor.iss`
- Modify: same file.

- [ ] **Step 1: Rename**

Run: `git mv crates/zed/resources/windows/zed.iss crates/zed/resources/windows/spk-editor.iss`

- [ ] **Step 2: Inspect**

Run: `head -80 crates/zed/resources/windows/spk-editor.iss`
Expected: an Inno Setup script with `[Setup]` section containing `AppName`, `AppPublisher`, `AppPublisherURL`, `AppId`, `DefaultDirName`, `OutputBaseFilename`, etc.

- [ ] **Step 3: Edit `[Setup]` keys**

Replace:
- `AppName=Zed` → `AppName=SPK Editor`
- `AppPublisher=Zed Industries, Inc.` → `AppPublisher=Simonov Pavel`
- `AppPublisherURL=…zed.dev…` → `AppPublisherURL=https://github.com/Sipaha/spk-editor`
- `AppSupportURL=…zed.dev…` (if present) → `AppSupportURL=https://github.com/Sipaha/spk-editor/issues`
- `AppUpdatesURL=…` (if present) → `AppUpdatesURL=https://github.com/Sipaha/spk-editor/releases`
- `AppId={{…}}` → `AppId={{ru.sipaha.spk-editor}}` (preserve the `{{}}` braces; Inno wants a stable GUID — the literal `ru.sipaha.spk-editor` as the id is acceptable, Inno uses the string verbatim).
- `DefaultDirName={autopf}\Zed` (or similar) → `DefaultDirName={autopf}\spk-editor`
- `DefaultGroupName=Zed` → `DefaultGroupName=SPK Editor`
- `OutputBaseFilename=ZedUserSetup-…` → `OutputBaseFilename=SpkEditorUserSetup-…`
- `UninstallDisplayName=Zed` → `UninstallDisplayName=SPK Editor`
- `UninstallDisplayIcon=…\zed.exe` → `UninstallDisplayIcon=…\spk-editor.exe`
- `SetupIconFile=…\app-icon.ico` — leave (icon path is structural; icon file itself replaced in Phase D).
- Any `[Files]` / `[Icons]` entries referencing `zed.exe` → `spk-editor.exe`. Reference to `zed.sh` (if present) → `spk-editor.sh`.
- Any `URL` / `Filename` reference to the channel application → `spk-editor`.

For per-channel variants (Preview / Nightly / Dev), if the same `.iss` parameterizes the channel, update the suffix logic to produce `ru.sipaha.spk-editor-preview` etc.; if there are separate `.iss` files per channel, plan a follow-up rename for them in this task (extend Step 1 / 2 / 3 accordingly).

- [ ] **Step 4: Find external references**

Run: `grep -rn 'zed\.iss' --include='*.sh' --include='*.ps1' .`
Expected: hit in `script/bundle-windows.ps1` (recorded for Task 15).

- [ ] **Step 5: Commit**

```bash
git add crates/zed/resources/windows/spk-editor.iss
git commit -m "Rename Inno Setup script and rebrand AppName, AppId, paths"
```

---

## Task 9: Rename Windows shell wrapper

**Files:**
- Rename: `crates/zed/resources/windows/zed.sh` → `crates/zed/resources/windows/spk-editor.sh`
- Modify: same file (if it references the binary name or paths).

- [ ] **Step 1: Inspect**

Run: `cat crates/zed/resources/windows/zed.sh`
Expected: a small shell wrapper, possibly invoking `zed.exe` or setting `PATH`.

- [ ] **Step 2: Rename**

Run: `git mv crates/zed/resources/windows/zed.sh crates/zed/resources/windows/spk-editor.sh`

- [ ] **Step 3: Edit**

In the renamed file, replace any `zed.exe` → `spk-editor.exe` and any directory paths `…/Zed/…` or `…/zed/…` with `…/spk-editor/…`. Leave shebang and other shell mechanics untouched.

- [ ] **Step 4: Find external references**

Run: `grep -rn 'zed\.sh' --include='*.iss' --include='*.ps1' --include='*.sh' .`
Expected: possible hit in `spk-editor.iss` already updated in Task 8 (verify the reference now uses the new name).

- [ ] **Step 5: Commit**

```bash
git add crates/zed/resources/windows/spk-editor.sh
git commit -m "Rename Windows zed.sh to spk-editor.sh"
```

---

## Task 10: Update Windows installer messages

**Files:**
- Modify: `crates/zed/resources/windows/messages/*.isl`

- [ ] **Step 1: Find user-visible Zed strings**

Run: `grep -n 'Zed' crates/zed/resources/windows/messages/*.isl`
Expected: hits inside `[CustomMessages]` or other text sections (possibly install-prompt strings, shortcut labels, file-association descriptions). The default Inno Setup catalog strings (license / next / cancel) should NOT contain "Zed" — only the customisations do.

- [ ] **Step 2: Replace**

For each hit:
- `Zed` (display name) → `SPK Editor`
- `zed` (lowercase, used as binary / scheme) → `spk-editor`

Preserve any `Zed Industries, Inc.` occurrence inside copyright lines (license-compliance).

- [ ] **Step 3: Verify**

Run: `grep -n 'Zed' crates/zed/resources/windows/messages/*.isl`
Expected: only matches inside copyright lines (or no matches at all, depending on what was in the file).

- [ ] **Step 4: Commit**

```bash
git add crates/zed/resources/windows/messages
git commit -m "Rebrand Windows installer messages to SPK Editor"
```

---

## Task 11: Update flatpak / snap manifests

**Files:**
- Modify: `crates/zed/resources/flatpak/*` and `crates/zed/resources/snap/*` (whatever is present).

- [ ] **Step 1: Inspect**

Run: `ls crates/zed/resources/flatpak crates/zed/resources/snap 2>/dev/null && find crates/zed/resources/flatpak crates/zed/resources/snap -type f 2>/dev/null`
Expected: flatpak `.yml` / `.json` manifest, possibly an AppStream metainfo `.xml`; snap `snapcraft.yaml`.

- [ ] **Step 2: Edit each manifest**

Replace:
- App id: `dev.zed.Zed` → `ru.sipaha.spk-editor` (in both flatpak `app-id` / `id` and snap `name` keys).
- Display name (`name`, `summary`, `Name=`) → `SPK Editor`.
- Description fields containing `Zed is …` → `SPK Editor is a personal fork of Zed by Zed Industries, Inc., modified by Simonov Pavel.`
- `command:` / `Exec=` → `spk-editor`.
- Icon names → `spk-editor`.
- Source URLs / homepage / bug tracker → `https://github.com/Sipaha/spk-editor`.

- [ ] **Step 3: Validate YAML / XML**

For each modified file, run a parser sanity check:
- YAML: `python3 -c 'import yaml,sys; yaml.safe_load(open(sys.argv[1]))' <path>`
- XML / JSON: `xmllint --noout <path>` or `python3 -m json.tool <path> >/dev/null`.

Expected: no parse errors.

- [ ] **Step 4: Commit**

```bash
git add crates/zed/resources/flatpak crates/zed/resources/snap
git commit -m "Rebrand flatpak and snap manifests to SPK Editor"
```

(Skip the commit if no flatpak / snap files exist; close the task as no-op.)

---

## Task 12: Update workspace + zed crate Cargo metadata

**Files:**
- Modify: `Cargo.toml` (workspace root), `crates/zed/Cargo.toml`.

- [ ] **Step 1: Inspect**

Run: `grep -n 'name\|description\|publish\|metadata' Cargo.toml crates/zed/Cargo.toml | head -40`
Expected: workspace metadata (`description`, `repository`, `homepage`, possibly `[workspace.metadata.bundle]`), and the `zed` crate's own `[package]` section.

- [ ] **Step 2: Edit workspace `Cargo.toml`**

Replace (only if these keys exist; otherwise skip):
- `description = "Zed …"` → `description = "SPK Editor — a personal fork of Zed by Zed Industries, Inc."`
- `repository = "https://github.com/zed-industries/zed"` → `repository = "https://github.com/Sipaha/spk-editor"`
- `homepage = "https://zed.dev"` → `homepage = "https://github.com/Sipaha/spk-editor"`
- `documentation = "…"` — leave pointing at upstream Zed docs.
- `[workspace.package].license` — leave unchanged.
- Authors list — leave unchanged (we're not removing Zed authorship).

- [ ] **Step 3: Edit `crates/zed/Cargo.toml`**

Same fields if present in this crate's `[package]`. Do **not** change `name = "zed"`. Do not change `license = "GPL-3.0-or-later"`.

- [ ] **Step 4: Inspect bundle metadata**

Run: `grep -n 'metadata.bundle\|product_name\|identifier' crates/zed/Cargo.toml`
Expected: zero or more `[package.metadata.*]` sections used by bundling tooling. If present, update `product_name` to `SPK Editor` and any `identifier` to `ru.sipaha.spk-editor`.

- [ ] **Step 5: Verify build still works**

Run: `cargo metadata --no-deps -q >/dev/null`
Expected: no error. Then: `cargo build -p zed --quiet`. Expected: builds.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/zed/Cargo.toml
git commit -m "Rebrand workspace and zed crate Cargo metadata"
```

---

# Phase C — Build scripts

## Task 13: Update `script/bundle-linux`

**Files:**
- Modify: `script/bundle-linux`

- [ ] **Step 1: Inspect**

Run: `grep -n 'zed\|Zed' script/bundle-linux`
Expected: references to `zed.desktop.in`, output filenames (`zed-linux-*.tar.gz` / `Zed-linux-*.tar.gz`), install paths (`/opt/zed`, `/usr/local/bin/zed`).

- [ ] **Step 2: Edit references**

Replace each user-visible reference:
- `zed.desktop.in` → `spk-editor.desktop.in` (file rename happened in Task 4).
- Output artifact name: `zed-linux-*.tar.gz` → `spk-editor-linux-*.tar.gz`.
- Output binary inside the tarball: `zed` → `spk-editor` (the cargo build still produces `zed` per the unchanged crate name; this rename is at the bundling step — copy/move the built binary to `spk-editor`).
- Install paths: `/opt/zed` → `/opt/spk-editor`, `/usr/local/bin/zed` → `/usr/local/bin/spk-editor`.
- `.desktop` substitutions (if the script does `sed s/@APP_NAME@/Zed/g`): values → `SPK Editor` and `spk-editor`.
- Icon source paths: update to whatever final filename is settled in Phase D (use `spk-editor.png` / `spk-editor-symbolic.svg` etc.). If the path remains `assets/icons/app/app-icon.png` (no rename of the asset itself), leave the path; only update the destination filename.

Leave any internal cargo invocation that targets `-p zed` unchanged.

- [ ] **Step 3: Smoke check**

Run: `bash -n script/bundle-linux`
Expected: no syntax errors. (We do not actually invoke the script here — just sanity-check it parses.)

- [ ] **Step 4: Commit**

```bash
git add script/bundle-linux
git commit -m "bundle-linux: Produce spk-editor artifacts and install paths"
```

---

## Task 14: Update `script/bundle-mac`

**Files:**
- Modify: `script/bundle-mac`

- [ ] **Step 1: Inspect**

Run: `grep -n 'Zed\|zed\.entitlements\|Zed\.app' script/bundle-mac`
Expected: references to `Zed.app`, `zed.entitlements`, plist generation, output `.dmg` filename.

- [ ] **Step 2: Edit references**

Replace:
- `Zed.app` → `SPK Editor.app` (every occurrence — there will be many: directory creation, codesign target, dmg input, etc.).
- `zed.entitlements` → `spk-editor.entitlements`.
- Output dmg name: `Zed-*.dmg` → `spk-editor-*.dmg`.
- Inside-bundle binary path: `Zed.app/Contents/MacOS/zed` → `SPK Editor.app/Contents/MacOS/spk-editor` (rename the binary inside the bundle).
- If the script generates `Info.plist` inline (heredoc), apply Task 6 changes here: bundle id, executable name, display name, URL scheme entries.
- Codesign-related logic (notarization, signing identities): keep all checks as-is, but if any code path would fail without a Zed-controlled signing identity, wrap that branch in a check for an environment variable (e.g. `if [ -n "$SPK_EDITOR_SIGN" ]; then …`). Default path produces an unsigned bundle.

- [ ] **Step 3: Smoke check**

Run: `bash -n script/bundle-mac`
Expected: no syntax errors.

- [ ] **Step 4: Commit**

```bash
git add script/bundle-mac
git commit -m "bundle-mac: Produce SPK Editor.app and skip codesign by default"
```

---

## Task 15: Update `script/bundle-windows.ps1`

**Files:**
- Modify: `script/bundle-windows.ps1`

- [ ] **Step 1: Inspect**

Run: `grep -n 'Zed\|zed\.iss\|zed\.exe' script/bundle-windows.ps1`
Expected: references to `zed.iss`, `zed.exe`, output installer name.

- [ ] **Step 2: Edit references**

Replace:
- `zed.iss` → `spk-editor.iss`.
- `zed.exe` → `spk-editor.exe`.
- Output installer name: `ZedSetup-*.exe` (or similar) → `SpkEditorSetup-*.exe` to match `OutputBaseFilename` set in Task 8.
- Any signing logic: same treatment as Task 14 — wrap in env-var guard, default to unsigned.

- [ ] **Step 3: Smoke check**

Run: `pwsh -NoProfile -Command "Get-Command -Syntax script/bundle-windows.ps1"` (PowerShell not required on Linux; if missing, skip and verify manually by reading).
Expected: no parse error, or "command not found" if pwsh is not installed (acceptable — the script's grammar is verified visually).

- [ ] **Step 4: Commit**

```bash
git add script/bundle-windows.ps1
git commit -m "bundle-windows: Produce spk-editor installer and binary"
```

---

## Task 16: Update `script/install.sh` and `script/uninstall.sh`

**Files:**
- Modify: `script/install.sh`, `script/uninstall.sh`

- [ ] **Step 1: Inspect both**

Run: `grep -n 'Zed\|zed' script/install.sh script/uninstall.sh`
Expected: references to install paths (`/usr/local/bin/zed`), `.desktop` filenames, sometimes downloads from `zed.dev`.

- [ ] **Step 2: Edit `install.sh`**

Replace:
- All `zed` binary path / symlink references → `spk-editor`.
- `.desktop` filename → `spk-editor.desktop`.
- Any download URL of upstream Zed releases → **remove the auto-download path entirely** (we do not publish releases). Replace with a friendly error message: `echo "spk-editor must be built from source. See https://github.com/Sipaha/spk-editor for instructions."; exit 1`. If the script has a `--from-source` mode, prefer that as the only path.
- Channel handling (`stable`, `preview`, `nightly`, `dev`): leave the channel-detection logic; only the URLs / paths change.

- [ ] **Step 3: Edit `uninstall.sh`**

Mirror Step 2: replace `zed` → `spk-editor` in paths to remove. Remove any upstream-server-talking logic (cleanup of update channels etc.) if it relies on Zed servers.

- [ ] **Step 4: Smoke check**

Run: `bash -n script/install.sh && bash -n script/uninstall.sh`
Expected: no syntax errors.

- [ ] **Step 5: Commit**

```bash
git add script/install.sh script/uninstall.sh
git commit -m "install/uninstall: Target spk-editor paths; drop upstream download"
```

---

# Phase D — Placeholder icons

## Task 17: Create placeholder icon generator script

**Files:**
- Create: `script/generate-placeholder-icons.sh`

- [ ] **Step 1: Verify ImageMagick and other tools are available**

Run: `command -v convert && command -v magick 2>/dev/null; command -v inkscape 2>/dev/null; command -v png2icns 2>/dev/null`
Expected: at least `convert` (ImageMagick legacy) or `magick` (ImageMagick 7+) is present. If not, the script's first step is to fail with a clear message.

- [ ] **Step 2: Write the script**

Create `script/generate-placeholder-icons.sh` with this content:

```bash
#!/usr/bin/env bash
# Generates placeholder icons for SPK Editor (a single 'S' on a colored
# background) in every size / format the project needs. Replace the output
# files with proper artwork later; this script is the single source of truth
# for placeholder geometry.

set -euo pipefail

if command -v magick >/dev/null 2>&1; then
    IM=magick
elif command -v convert >/dev/null 2>&1; then
    IM=convert
else
    echo "Need ImageMagick (magick or convert)." >&2
    exit 1
fi

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

BG=#2563EB
FG=#FFFFFF

render() {
    local size="$1"
    local out="$2"
    "$IM" -size "${size}x${size}" "xc:$BG" \
        -gravity center -fill "$FG" -font 'DejaVu-Sans-Bold' \
        -pointsize $((size * 6 / 10)) -annotate 0 'S' "$out"
}

# PNG variants (Linux desktop, macOS retina source)
mkdir -p "$ROOT/assets/icons/app"
for variant in '' '-dev' '-preview' '-nightly'; do
    render 512  "$ROOT/assets/icons/app/app-icon${variant}.png"
    render 1024 "$ROOT/assets/icons/app/app-icon${variant}@2x.png"
done

# macOS .icns (Document.icns is the per-document file; same placeholder)
if command -v png2icns >/dev/null 2>&1; then
    render 1024 "$TMP/document.png"
    png2icns "$ROOT/assets/icons/app/Document.icns" "$TMP/document.png"
fi

# Windows .ico — pack 16, 32, 48, 64, 128, 256
mkdir -p "$ROOT/crates/zed/resources/windows"
for variant in '' '-dev' '-preview' '-nightly'; do
    sizes=()
    for s in 16 32 48 64 128 256; do
        f="$TMP/ico-${s}${variant}.png"
        render "$s" "$f"
        sizes+=("$f")
    done
    "$IM" "${sizes[@]}" "$ROOT/crates/zed/resources/windows/app-icon${variant}.ico"
done

echo "Placeholder icons regenerated. Replace with real artwork when ready."
```

- [ ] **Step 3: Make executable**

Run: `chmod +x script/generate-placeholder-icons.sh`

- [ ] **Step 4: Commit (script only, before icons regenerated)**

```bash
git add script/generate-placeholder-icons.sh
git commit -m "Add placeholder icon generation script"
```

---

## Task 18: Replace icon assets with placeholders

**Files:**
- Modify (overwrite): `assets/icons/app/app-icon{,@2x,-dev,-dev@2x,-preview,-preview@2x,-nightly,-nightly@2x}.png`, `assets/icons/app/Document.icns`, `crates/zed/resources/windows/app-icon{,-dev,-preview,-nightly}.ico`.
- Optionally: `assets/images/zed_logo.svg` (minimal placeholder SVG).

- [ ] **Step 1: Run the generator**

Run: `./script/generate-placeholder-icons.sh`
Expected: prints "Placeholder icons regenerated." Files in the listed paths are overwritten.

- [ ] **Step 2: Replace `assets/images/zed_logo.svg` with a minimal placeholder**

Overwrite `assets/images/zed_logo.svg` with:

```xml
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64">
  <rect width="64" height="64" fill="#2563EB"/>
  <text x="32" y="44" text-anchor="middle"
        font-family="sans-serif" font-size="40" font-weight="bold" fill="#FFFFFF">S</text>
</svg>
```

(Filename is preserved — `zed_logo.svg` — because internal code paths reference it. The contents are replaced.)

- [ ] **Step 3: Verify a sample**

Run: `file assets/icons/app/app-icon.png crates/zed/resources/windows/app-icon.ico`
Expected: `PNG image data, 512 x 512 …` and `MS Windows icon resource …`.

- [ ] **Step 4: Commit**

```bash
git add assets/icons/app crates/zed/resources/windows/app-icon*.ico assets/images/zed_logo.svg
git commit -m "Replace app icons with SPK placeholder artwork"
```

(The other `assets/icons/zed_*.svg` files referenced by Zeta / native agent UI are left untouched — they belong to disabled subsystems, never reach the user.)

---

# Phase E — Service disablement

Each disablement task follows the same shape: locate the init / dispatch site, comment out or guard the call, build, run, verify. Internal types, modules, and crates are not removed (merge-friendliness).

## Task 19: Disable `auto_update`

**Files:**
- Modify: `crates/zed/src/zed.rs` (the `init` function or wherever `auto_update::init` is called).

- [ ] **Step 1: Locate**

Run: `grep -rn 'auto_update::init\|auto_update_helper\|auto_update_ui::init' crates/zed/src/`
Expected: one or more init-site call(s).

- [ ] **Step 2: Disable the init**

For each call, replace it with a no-op + comment, e.g.:

```rust
// Auto-update disabled in spk-editor: no upstream channel, builds from source.
// auto_update::init(http_client.clone(), cx);
```

If the call is part of a larger expression (e.g. assigning a returned handle), guard with `if false` or hoist the binding to `None` per the surrounding type. Adapt to the actual code shape; do not introduce panic or silent failure.

- [ ] **Step 3: Hide auto-update UI affordances**

Run: `grep -rn 'auto_update_ui\|AutoUpdate\|"Check for Updates"' crates/zed/src/ crates/title_bar/ crates/menu/`
Expected: menu items, title-bar buttons. For each user-facing UI affordance pointing at update-check, remove the `register_action` / menu entry. Keep the action types defined (other code may still register handlers harmlessly).

- [ ] **Step 4: Build**

Run: `cargo build -p zed --quiet`
Expected: builds without errors. If the disabled init returned a handle that is consumed downstream and the build now fails, replace the value with the type's `None` or default equivalent — do **not** delete the consumer.

- [ ] **Step 5: Run clippy**

Run: `./script/clippy -p zed`
Expected: no new warnings (a `dead_code` warning on the still-imported `auto_update` symbol is acceptable; suppress with `#[allow(unused_imports)]` on the import if needed).

- [ ] **Step 6: Commit**

```bash
git add crates/zed/src/zed.rs <other files touched>
git commit -m "Disable auto_update: no upstream channel for spk-editor"
```

---

## Task 20: Disable `telemetry` end-to-end

**Files:**
- Modify: `crates/telemetry/src/telemetry.rs` (or `crates/client/src/telemetry.rs` — locate first), plus default settings file (likely `assets/settings/default.json` or similar).

- [ ] **Step 1: Locate the dispatch site**

Run: `grep -rn 'fn report_event\|fn send_event\|fn flush\|TELEMETRY_ENDPOINT\|telemetry\.zed\.dev' crates/telemetry crates/client`
Expected: a function that POSTs events. Note the function name and module path.

- [ ] **Step 2: Hard-disable at dispatch**

In the dispatched function (e.g. `report_event`), make the body an early return:

```rust
pub fn report_event(/* original args */) {
    // spk-editor: telemetry is permanently disabled. No events are sent.
    return;
}
```

Apply the same to any `flush`, `send_*`, or HTTP-POST entrypoint inside the telemetry module. If multiple entrypoints exist, give each the same early-return.

- [ ] **Step 3: Default settings opt-out**

Run: `grep -rn '"telemetry"' assets/settings/ assets/keymaps/ 2>/dev/null` (also check `crates/settings/`).
Expected: a default settings file (likely `assets/settings/default.json`) that mentions telemetry. Edit the defaults to explicitly disable:

```json
"telemetry": {
    "diagnostics": false,
    "metrics": false
},
```

If the keys are different (e.g. `enabled: false`), use the actual schema observed in the file.

- [ ] **Step 4: Build + clippy**

Run: `cargo build --quiet && ./script/clippy -p telemetry`
Expected: builds; no warnings. The `report_event`'s args may now be unused — prefix them with `_` to silence.

- [ ] **Step 5: Commit**

```bash
git add <files touched>
git commit -m "Hard-disable telemetry dispatch and flip defaults to off"
```

---

## Task 21: Disable `collab` and `collab_ui`

**Files:**
- Modify: `crates/zed/src/zed.rs` (init site for `collab_ui`), plus any title-bar / menu registrations.

- [ ] **Step 1: Locate init**

Run: `grep -rn 'collab_ui::init\|collab::init\|chat_panel\|notification_panel\|channels_panel' crates/zed/src/`
Expected: the call(s) that bring up collab panels.

- [ ] **Step 2: Skip init**

Comment out the `collab_ui::init(...)` call (and any panel-registration calls dependent on it):

```rust
// Collab is disabled in spk-editor (no Zed Industries collab server access).
// collab_ui::init(...);
```

- [ ] **Step 3: Hide panels and contacts**

Run: `grep -rn 'CollabPanel\|ContactList\|"Open Channels"\|"Add Contact"' crates/title_bar crates/zed/src crates/menu`
Expected: UI registrations of contacts and channels. Remove the registrations (so the user cannot open these panels via menu / keymap default). Do not delete the panel types themselves.

- [ ] **Step 4: Build + clippy**

Run: `cargo build --quiet && ./script/clippy --workspace --no-deps`
Expected: no errors. Unused-import warnings on `collab_ui` are acceptable; suppress narrowly with `#[allow(unused_imports)]` if needed.

- [ ] **Step 5: Commit**

```bash
git add <files touched>
git commit -m "Disable collab_ui init and hide collab panels"
```

---

## Task 22: Hide sign-in / Zed account UI

**Files:**
- Modify: `crates/title_bar/src/` (sign-in button), `crates/onboarding/`, `crates/ai_onboarding/`, anywhere "Sign in to Zed" appears.

- [ ] **Step 1: Locate sign-in surfaces**

Run: `grep -rn '"Sign In"\|"Sign in"\|"Sign Up"\|sign_in\|SignIn\|Authenticate Zed' crates/title_bar crates/onboarding crates/ai_onboarding crates/zed/src`
Expected: button definitions, onboarding screens, action handlers.

- [ ] **Step 2: Remove user-facing entry points**

For each visible button / menu item / onboarding step that says "Sign in to Zed" or equivalent, remove the registration or wrap it with `if false { ... }`. Keep the underlying `client::authenticate` function intact — nothing will call it.

- [ ] **Step 3: Adjust onboarding flow**

If onboarding has a "Sign in to Zed" step that is not optional, replace that step with a skip or remove it from the step list. Welcome screen (Phase F Task 30) will be cleaned of "Sign in" calls-to-action there.

- [ ] **Step 4: Build + clippy**

Run: `cargo build --quiet && ./script/clippy -p zed -p onboarding -p ai_onboarding -p title_bar`
Expected: no errors.

- [ ] **Step 5: Commit**

```bash
git add <files touched>
git commit -m "Hide sign-in UI affordances; spk-editor does not use Zed accounts"
```

---

## Task 23: Disable native cloud LLM provider in agent UI

**Files:**
- Modify: `crates/agent_ui/src/agent_registry_ui.rs` (provider list), `crates/language_models/src/provider/cloud.rs` (provider registration), `crates/agent/src/` (thread provider).

- [ ] **Step 1: Locate registrations**

Run: `grep -rn 'CloudLanguageModelProvider\|register.*provider\|"Zed".*provider\|cloud_llm_client::init' crates/agent crates/agent_ui crates/language_models`
Expected: a place that registers the Zed cloud provider into a global model-provider registry.

- [ ] **Step 2: Skip cloud provider registration**

Wrap the `register_cloud_provider(…)` call (or equivalent) in `if false {` … `}` with a comment:

```rust
// spk-editor: do not register Zed cloud LLM provider. We rely on external
// agents (Claude Code) via ACP for AI features.
if false {
    cloud_provider::register(...);
}
```

- [ ] **Step 3: Hide native Zed thread option in agent UI**

Inspect `crates/agent_ui/src/agent_registry_ui.rs` (ACP / native split). If it lists a "Native" or "Zed-managed" thread option distinct from the ACP options, remove that list entry. Keep the ACP entries (Claude Code, Gemini, etc.).

- [ ] **Step 4: Build + clippy**

Run: `cargo build --quiet && ./script/clippy -p agent -p agent_ui -p language_models`
Expected: no errors.

- [ ] **Step 5: Commit**

```bash
git add <files touched>
git commit -m "Hide native cloud LLM provider; keep ACP external agents"
```

---

## Task 24: Disable Zeta edit prediction

**Files:**
- Modify: `crates/zed/src/zed.rs` (init site), `crates/edit_prediction_ui/src/` (UI registrations), default settings.

- [ ] **Step 1: Locate**

Run: `grep -rn 'zeta::init\|zeta_prompt\|EditPredictionProvider::Zed\|"zeta"' crates/zed/src crates/edit_prediction_ui crates/zeta_prompt`
Expected: init calls and a provider enum.

- [ ] **Step 2: Skip init**

Comment out `zeta::init(...)` (and any `EditPrediction::register(Zed, …)`) with a comment:

```rust
// Edit prediction (Zeta) disabled in spk-editor (requires Zed account).
```

- [ ] **Step 3: Hide UI**

In `crates/edit_prediction_ui/`, remove or guard the menu / status-bar widget that surfaces the Zeta provider. Keep code paths for other providers (e.g. Copilot, Supermaven) intact if present.

- [ ] **Step 4: Default settings**

Edit the default settings (same file as Task 20):

```json
"edit_predictions": {
    "provider": "none"
}
```

(Use the exact key observed in the existing defaults — could be `"edit_prediction.provider"` etc.)

- [ ] **Step 5: Build + clippy**

Run: `cargo build --quiet && ./script/clippy -p zed -p edit_prediction_ui`
Expected: no errors.

- [ ] **Step 6: Commit**

```bash
git add <files touched>
git commit -m "Disable Zeta edit prediction; default provider = none"
```

---

## Task 25: Disable Sentry crash upload

**Files:**
- Modify: `crates/zed/src/reliability.rs`

- [ ] **Step 1: Locate the upload entrypoint**

Run: `grep -n 'fn report_panic\|fn upload\|sentry\.io\|api.zed.dev/crashes\|crash_endpoint\|CRASH' crates/zed/src/reliability.rs`
Expected: a function (probably named something like `upload_panic`, `send_crash_report`, or a closure inside `init_panic_hook`) that constructs the multipart sentry form and POSTs it.

- [ ] **Step 2: Early-return in upload**

Make the upload function return immediately, but **keep the local panic logging**:

```rust
fn upload_panic(/* args */) {
    // spk-editor: crash uploads are disabled. Panics are still written
    // to ~/.local/state/spk-editor/logs/ for the user to inspect or report
    // manually at https://github.com/Sipaha/spk-editor/issues.
    return;
}
```

If the file's top-level constants include a Sentry DSN / endpoint URL, replace the value with an empty string to make any other accidental call inert:

```rust
// spk-editor: no crash-report endpoint.
const CRASH_REPORT_URL: &str = "";
```

(Adapt the constant name to the actual one observed.)

- [ ] **Step 3: Verify local panic logging still happens**

Inspect the `init_panic_hook`-style function: confirm it still writes to disk (to the path produced by `paths::*`). If the disk-write path is gated on the upload succeeding (unlikely but possible), restructure so that disk logging always happens and upload is a no-op.

- [ ] **Step 4: Build + clippy**

Run: `cargo build --quiet && ./script/clippy -p zed`
Expected: no errors.

- [ ] **Step 5: Commit**

```bash
git add crates/zed/src/reliability.rs
git commit -m "Disable Sentry crash upload; keep local panic logs"
```

---

## Task 26: Update `feedback` URLs

**Files:**
- Modify: `crates/feedback/src/feedback.rs` (line 18 / 20 / 25 / 37 area).

- [ ] **Step 1: Inspect**

Run: `grep -n 'zed-industries\|zed\.dev\|hi@zed' crates/feedback/src/feedback.rs`
Expected: hits at lines 18, 20, 25, 37 (per the earlier exploration).

- [ ] **Step 2: Edit URLs**

```rust
const ZED_REPO_URL: &str = "https://github.com/Sipaha/spk-editor";
const REQUEST_FEATURE_URL: &str = "https://github.com/Sipaha/spk-editor/discussions/new/choose";
// File-issue URL:
"https://github.com/Sipaha/spk-editor/issues/new"
```

For the `mailto:hi@zed.dev` line: replace the entire `mailto:` URL with the issues URL — i.e. the email-feedback action becomes a "file an issue" action. Update the action's user-visible label too if it says "Email Zed Team": rename to "Open issue on GitHub".

(The constant name `ZED_REPO_URL` is internal and can stay — it's not user-visible. Leave the identifier; only update the value. If you prefer to rename the constant for clarity, that is acceptable but optional and not required by the spec.)

- [ ] **Step 3: Build + clippy**

Run: `cargo build --quiet && ./script/clippy -p feedback`
Expected: no errors.

- [ ] **Step 4: Commit**

```bash
git add crates/feedback/src/feedback.rs
git commit -m "feedback: Point links at github.com/Sipaha/spk-editor"
```

---

# Phase F — Runtime user-visible strings

## Task 27: Audit and replace remaining `"Zed"` literals

**Files:**
- Modify: scattered files in `crates/`. The audit reveals which.

This task is the largest and most error-prone in the plan. Treat each grep hit individually — do not bulk replace.

- [ ] **Step 1: Run the audit**

Run:
```
grep -rn '"Zed"\|"zed"' crates/ \
  --include='*.rs' \
  | grep -v 'crates/zed/Cargo.toml' \
  | grep -v 'crates/paths/src/paths.rs' \
  | grep -v 'crates/release_channel/src/lib.rs' \
  | grep -v 'crates/feedback/src/feedback.rs' \
  > /tmp/zed-literal-audit.txt
wc -l /tmp/zed-literal-audit.txt
cat /tmp/zed-literal-audit.txt
```

Expected: a list of remaining literal occurrences. Each must be classified as one of:
- **User-visible label / message** → replace with `"SPK Editor"` (or pull `display_name()` from `release_channel` if convenient and at runtime).
- **Internal identifier** (crate / module / type / enum-variant string discriminator, settings-file key) → **leave unchanged**.
- **License / copyright / attribution** → leave unchanged.
- **Test fixture / docstring** → leave unchanged unless user-visible (e.g. comment in error message that bubbles up).

- [ ] **Step 2: Walk each hit and apply the classification**

For each line in the audit output, open the file, read the surrounding context, decide the bucket, and either edit or skip. Keep notes (in your head — no need to commit a doc) so you can split commits cleanly.

- [ ] **Step 3: Re-run the audit**

Run: `grep -rn '"Zed"\|"zed"' crates/ --include='*.rs' | grep -v <skip patterns above> > /tmp/zed-literal-audit2.txt; wc -l /tmp/zed-literal-audit2.txt`
Expected: substantially fewer lines; remaining lines are all in the "leave unchanged" buckets.

- [ ] **Step 4: Build + clippy**

Run: `cargo build --quiet && ./script/clippy --workspace --no-deps`
Expected: no errors.

- [ ] **Step 5: Commit (single commit summarizing the sweep)**

```bash
git add -u
git commit -m "Replace user-visible Zed string literals with SPK Editor"
```

---

## Task 28: Update About dialog

**Files:**
- Modify: `crates/zed/src/zed.rs` (the action handler for "About" / "OpenAbout" — locate by `register_action::<About>` or similar).

- [ ] **Step 1: Locate the About handler**

Run: `grep -rn 'About\b\|OpenAbout\|fn open_about' crates/zed/src/`
Expected: an action handler that opens a dialog or window with the About content.

- [ ] **Step 2: Set the About body**

Replace the body string with:

```rust
let about_text = format!(
    "SPK Editor v{} ({})\n\
     \n\
     Fork of Zed by Zed Industries, Inc., modified by Simonov Pavel.\n\
     Source: https://github.com/Sipaha/spk-editor\n\
     \n\
     Distributed under GPL-3.0-or-later (editor), AGPL-3.0 (collab), \
     Apache-2.0 (libraries).",
    env!("CARGO_PKG_VERSION"),
    option_env!("ZED_COMMIT_SHA").unwrap_or("dev")
);
```

If the existing About handler reads from a struct (e.g. `AboutInfo`) rather than a free-form string, populate the struct fields with the equivalent values; do not invent new fields. The exact assembly will depend on the existing GPUI dialog code — adapt the template to match.

- [ ] **Step 3: Build + clippy**

Run: `cargo build --quiet && ./script/clippy -p zed`
Expected: no errors.

- [ ] **Step 4: Commit**

```bash
git add crates/zed/src/zed.rs
git commit -m "Show spk-editor identity and Zed attribution in About dialog"
```

---

## Task 29: Update CLI help / argparse

**Files:**
- Modify: `crates/cli/src/cli.rs` (or wherever clap is configured), `crates/zed/src/main.rs` (if the binary has its own argparse).

- [ ] **Step 1: Locate clap configuration**

Run: `grep -rn '#\[derive(Parser)\]\|Command::new\|clap' crates/cli crates/zed/src/main.rs`
Expected: a `#[derive(Parser)]` struct with `#[command(name = "...", about = "...")]` or `Command::new("zed")` chain.

- [ ] **Step 2: Update name and about**

Replace:
- `name = "zed"` → `name = "spk-editor"`
- `about = "Zed editor …"` → `about = "SPK Editor — fork of Zed by Zed Industries, Inc., modified by Simonov Pavel"`
- `version = …` — leave (auto-derived from `CARGO_PKG_VERSION`).

If subcommand help mentions Zed by name in user-visible text, replace there too.

- [ ] **Step 3: Build + clippy + smoke**

Run: `cargo build --quiet && ./script/clippy -p cli`
Expected: no errors.

Then run: `cargo run -p cli -- --help 2>&1 | head -20`
Expected: help text shows `spk-editor` and the `SPK Editor — fork of Zed …` about-line.

- [ ] **Step 4: Commit**

```bash
git add <files touched>
git commit -m "cli: Rename CLI to spk-editor and update about line"
```

---

## Task 30: Clean welcome / onboarding screens

**Files:**
- Modify: `crates/welcome/src/welcome.rs` (or equivalent), `crates/onboarding/src/`, `crates/ai_onboarding/src/`.

- [ ] **Step 1: Inspect**

Run: `grep -rn '"Sign in"\|"Sign up"\|"Zed"\|Zed Pro\|trial' crates/welcome crates/onboarding crates/ai_onboarding`
Expected: hard-coded onboarding copy, sign-in CTA buttons, "Zed Pro" upsells, trial banners.

- [ ] **Step 2: Remove sign-in / Zed Pro / trial sections**

For each onboarding screen / step / banner that promotes Zed cloud features (sign in, start trial, upgrade plan, Zed Pro), delete the registration. Keep the screen scaffolding if it has other useful content (theme picker, keymap import, open project).

- [ ] **Step 3: Replace remaining "Zed" mentions**

Walk the grep output for `"Zed"` literals: these are screen titles, paragraph copy ("Welcome to Zed"). Replace with "SPK Editor". Use `display_name()` from `release_channel` only if the surrounding code already imports it — do not introduce the dependency for a single string.

- [ ] **Step 4: Build + clippy + run**

Run: `cargo build --quiet && ./script/clippy -p welcome -p onboarding -p ai_onboarding`
Expected: no errors.

- [ ] **Step 5: Commit**

```bash
git add <files touched>
git commit -m "Welcome / onboarding: Remove Zed-cloud upsells and rebrand copy"
```

---

# Phase G — License & attribution

## Task 31: Rewrite `README.md`

**Files:**
- Modify (rewrite): `README.md`

- [ ] **Step 1: Inspect the current README structure**

Run: `head -60 README.md`
Expected: Zed-branded README with sections (Installation / Developing / Licensing / Sponsorship / etc.).

- [ ] **Step 2: Replace contents**

Overwrite `README.md` with:

```markdown
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

The shipped icon is a placeholder. Regenerate (and replace later with real artwork) via:

```sh
script/generate-placeholder-icons.sh
```

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
```

- [ ] **Step 3: Verify**

Run: `wc -l README.md && head -3 README.md`
Expected: ~80 lines; first heading is `# SPK Editor`.

- [ ] **Step 4: Commit**

```bash
git add README.md
git commit -m "Rewrite README for spk-editor with Zed attribution and license notes"
```

---

## Task 32: Replace `CONTRIBUTING.md`

**Files:**
- Modify (rewrite): `CONTRIBUTING.md`

- [ ] **Step 1: Overwrite**

Replace `CONTRIBUTING.md` contents with:

```markdown
# Contributing to SPK Editor

SPK Editor is a personal fork of Zed maintained by **Simonov Pavel**.

- **Bugs and feature requests specific to spk-editor** (rebrand issues, service-detachment regressions, integration with Claude Code): file an issue at <https://github.com/Sipaha/spk-editor/issues>.
- **Bugs in the Zed editor itself** (anything that also reproduces in upstream Zed): please file directly at <https://github.com/zed-industries/zed/issues>. SPK Editor merges from upstream periodically and your fix will reach this fork that way.
- **Pull requests to spk-editor**: welcome but evaluated case-by-case. Keep the diff against upstream small — that is the central design constraint of this fork. Contributions that touch internal identifiers (`crates/zed`, module names, type names) will likely be declined.

By contributing you agree that your contribution is licensed under the same license as the file you are modifying (`GPL-3.0-or-later` for editor code, `AGPL-3.0` for collab, `Apache-2.0` for libraries).
```

- [ ] **Step 2: Commit**

```bash
git add CONTRIBUTING.md
git commit -m "Replace CONTRIBUTING.md with personal-fork stub"
```

---

## Task 33: Reorganize `legal/` directory

**Files:**
- Move: `legal/{terms,privacy-policy,subprocessors,third-party-terms}.md` → `legal/upstream-zed/`
- Create: `legal/README.md`

- [ ] **Step 1: Create the subdirectory and move files**

Run:
```bash
mkdir -p legal/upstream-zed
git mv legal/terms.md legal/upstream-zed/terms.md
git mv legal/privacy-policy.md legal/upstream-zed/privacy-policy.md
git mv legal/subprocessors.md legal/upstream-zed/subprocessors.md
git mv legal/third-party-terms.md legal/upstream-zed/third-party-terms.md
```

- [ ] **Step 2: Create `legal/README.md`**

```markdown
# Legal Documents — Inherited from Upstream Zed

The files in `upstream-zed/` are the Terms of Service, Privacy Policy, Subprocessors list, and Third-Party Terms originally written by Zed Industries, Inc. for the Zed editor and its hosted services (collab, telemetry, sign-in, cloud LLM proxy).

**These documents do not apply to SPK Editor builds.** SPK Editor operates **no** service infrastructure: no telemetry endpoint, no collab server, no sign-in, no cloud LLM proxy, no auto-update server. There is no data processing relationship between the user and the maintainer (Simonov Pavel) created by using SPK Editor.

These documents are preserved in the tree to:
1. Honor the upstream copyright (per GPL §5(a) — modified versions retain attribution).
2. Make it clear which legal text belongs to upstream Zed and is not endorsed or re-issued by the SPK Editor maintainer.

If SPK Editor ever starts operating its own services in the future, separate legal documents will be added at this directory's top level alongside this README.
```

- [ ] **Step 3: Find references to the moved paths**

Run: `grep -rn 'legal/terms\|legal/privacy\|legal/subprocessors\|legal/third-party' --include='*.md' --include='*.rs' --include='*.toml' .`
Expected: hits in upstream README (already replaced in Task 31) and possibly inside the editor's UI (e.g. "Privacy Policy" link). For each remaining reference inside `crates/`, point it at `legal/upstream-zed/<file>.md` instead.

- [ ] **Step 4: Commit**

```bash
git add legal
git commit -m "Move upstream Zed legal docs into legal/upstream-zed and add explanatory README"
```

---

# Phase H — CI cleanup

## Task 34: Disable workflows depending on Zed-internal secrets

**Files:**
- Modify: each `.github/workflows/*.yml` that depends on Zed-controlled secrets.

- [ ] **Step 1: Audit**

Run: `ls .github/workflows/ && grep -ln 'secrets\.' .github/workflows/*.yml`
Expected: a list of workflows using secrets. For each, run `grep -n 'secrets\.' .github/workflows/<name>.yml` to see which secrets they need.

- [ ] **Step 2: Categorize each workflow**

For each workflow:
- **Useful and runnable in our fork without Zed secrets** (e.g. `cargo check`, lints, license audit, unit tests on push): leave enabled.
- **Useful but blocked on a Zed-only secret that we might revive later** (e.g. notarize-macos, sign-windows): keep the file but neutralize the trigger by setting `on: workflow_dispatch:` only (drops automatic runs but keeps the file ready to run manually if we later configure secrets).
- **Definitely not for us** (e.g. publish-to-zed.dev, notify-zed-Slack, deploy-collab-to-zed-cloud): set `on: workflow_dispatch:` and prepend each job with `if: false # spk-editor: not applicable` so it cannot accidentally fire.

Do not delete workflow files — keeping them in the tree minimizes upstream-merge conflicts.

- [ ] **Step 3: Edit per categorization**

For each workflow assigned a category in Step 2, apply the corresponding edit. Be conservative: when in doubt about a workflow's purpose, default to `if: false` rather than leaving it active and risking a noisy failure on every push.

- [ ] **Step 4: Verify YAML validity**

Run: `for f in .github/workflows/*.yml; do python3 -c "import yaml,sys; yaml.safe_load(open(sys.argv[1]))" "$f" || echo "BAD: $f"; done`
Expected: no `BAD:` lines.

- [ ] **Step 5: Commit**

```bash
git add .github/workflows
git commit -m "Neutralize CI workflows that depend on Zed Industries secrets"
```

---

# Phase I — Verification

## Task 35: Build, clippy, run smoke test

**Files:** none (verification only).

- [ ] **Step 1: Full build**

Run: `cargo build --release --quiet`
Expected: builds; produces `target/release/zed`. Note that the binary keeps its cargo-derived name `zed` — installation rename happens in the bundling scripts.

- [ ] **Step 2: Clippy**

Run: `./script/clippy --workspace --no-deps`
Expected: no warnings, no errors.

- [ ] **Step 3: Unit tests**

Run: `cargo test -p paths -p release_channel -p feedback -p telemetry -p zed --quiet`
Expected: all pass, including the rebrand tests added in Tasks 2–3.

- [ ] **Step 4: Run the binary, observe directory creation**

Run:
```bash
rm -rf ~/.config/spk-editor ~/.local/share/spk-editor ~/.cache/spk-editor
mv ~/.config/zed ~/.config/zed.bak 2>/dev/null || true
mv ~/.local/share/zed ~/.local/share/zed.bak 2>/dev/null || true
mv ~/.cache/zed ~/.cache/zed.bak 2>/dev/null || true

target/release/zed &
EDITOR_PID=$!
sleep 5
ls -la ~/.config/ ~/.local/share/ ~/.cache/ | grep -E 'spk-editor|zed'
kill $EDITOR_PID 2>/dev/null || true
```

Expected: `spk-editor/` directories created; **no** new `zed/` directories created. Restore the `.bak` directories afterward (or leave them if you no longer use upstream Zed).

- [ ] **Step 5: Manual UI checks (with the binary running)**

Confirm visually:
- Window title contains `SPK Editor`.
- About dialog shows: `SPK Editor v… Fork of Zed by Zed Industries, Inc., modified by Simonov Pavel.`
- Menu bar / command palette: no "Sign in to Zed", no "Check for updates", no collab panels.
- Extensions panel: opens, lists extensions from `zed.dev`.
- Agent panel: only ACP-based options (Claude Code etc.); no native Zed cloud thread option.
- Click "Give feedback" → opens browser at `https://github.com/Sipaha/spk-editor/issues`.

For each item that fails, file the failure as a fix-up commit before continuing to Task 36.

- [ ] **Step 6: Commit any fix-ups**

If Step 5 found issues that required code changes, commit each fix individually with a descriptive message.

---

## Task 36: Network audit

**Files:** none (verification only).

- [ ] **Step 1: Set up traffic capture**

Pick one:
- `mitmproxy` with the editor configured to use it as an HTTPS proxy (env vars `HTTPS_PROXY=http://localhost:8080`, plus the mitmproxy CA installed in the editor's trust store).
- `tcpdump` on the loopback / outbound interface, filtering by the editor's PID via `ss -p`.
- `strace -f -e trace=network` on the editor process.

The simplest for a smoke test is `strace`:

```bash
strace -f -e trace=connect -o /tmp/spk-editor-net.log target/release/zed &
sleep 30
killall -SIGINT zed
grep -E 'sin_addr|getaddrinfo|connect' /tmp/spk-editor-net.log | head -50
```

- [ ] **Step 2: Identify any hosts contacted**

From the trace, extract DNS lookups and TCP connect targets. Acceptable hosts:
- `zed.dev` (extension registry — only when the user opens the extensions panel or a download is in flight).
- `127.0.0.1` / `localhost` (LSP servers, language servers).
- The user's git remotes if they happen to fetch.

Forbidden hosts:
- `api.zed.dev`, `collab.zed.dev`, `telemetry.zed.dev`, `cloud.zed.dev`, `*.sentry.io`, `o*.ingest.sentry.io`.

If anything from the forbidden list shows up, find the call site (grep for the host string in `crates/`), fix it (early return / endpoint blank), commit the fix, and re-run this task.

- [ ] **Step 3: Confirm no unsolicited extension-registry traffic at idle**

Do a 30-second idle session (start the editor, leave it alone, do not open the extensions panel). Confirm no traffic to `zed.dev` happens during that window.

- [ ] **Step 4: Commit fix-ups (if any)**

If issues were found in Steps 2 / 3 and patched, commit each.

---

## Task 37: License audit

**Files:** none (verification only).

- [ ] **Step 1: Run cargo-about**

Run:
```bash
cargo install cargo-about --quiet 2>/dev/null || true
cargo about generate -c script/licenses/zed-licenses.toml templates/about.hbs > /dev/null
```

Expected: success. If it fails, fix per the instructions in (the rewritten) `README.md`'s Licensing section, commit the fix, and re-run.

- [ ] **Step 2: Verify upstream copyright preserved**

Run:
```bash
git show HEAD~38:LICENSE-GPL > /tmp/license-gpl-old.txt 2>/dev/null \
  || git show e2b02e2290:LICENSE-GPL > /tmp/license-gpl-old.txt
diff /tmp/license-gpl-old.txt LICENSE-GPL
diff <(git show e2b02e2290:LICENSE-AGPL) LICENSE-AGPL
diff <(git show e2b02e2290:LICENSE-APACHE) LICENSE-APACHE
```

Expected: all three diffs are empty.

- [ ] **Step 3: Verify attribution string is present**

Run: `grep -n 'modified by Simonov Pavel' README.md crates/zed/src/zed.rs`
Expected: hits in both files (README and the About handler).

---

## Task 38: Upstream merge dry-run

**Files:** none (verification only).

- [ ] **Step 1: Add the upstream remote (if not already configured)**

Run:
```bash
git remote -v | grep -q '^upstream' || git remote add upstream https://github.com/zed-industries/zed.git
git fetch upstream main --quiet
```

- [ ] **Step 2: Dry-run merge into a throwaway branch**

Run:
```bash
git checkout -b rebrand/upstream-merge-dryrun
git merge upstream/main --no-commit --no-ff || true
CONFLICTS=$(git diff --name-only --diff-filter=U)
echo "Conflict count: $(echo "$CONFLICTS" | grep -c .)"
echo "$CONFLICTS"
git merge --abort
git checkout rebrand/spk-editor
git branch -D rebrand/upstream-merge-dryrun
```

Expected: prints conflict count and file list. **Goal**: under 30 conflicting files, all in surface areas we touched intentionally (`crates/paths/src/paths.rs`, `crates/release_channel/src/lib.rs`, `crates/feedback/src/feedback.rs`, `crates/zed/src/zed.rs`, `README.md`, bundling scripts, plist / iss).

If unexpected files conflict (especially internal-identifier files that we deliberately did not rebrand), investigate — it likely means the surgical edits leaked into something that should have been left alone.

- [ ] **Step 3: Document the merge strategy**

Add a short section to `README.md` (just below "Upstream") documenting the conflict count from this run, e.g.:

```markdown
At the time of the initial rebrand commit, the upstream merge dry-run produced N conflicting files: <list>. Future merges should be expected to conflict primarily in the same set.
```

(Skip if already documented in some other way.)

- [ ] **Step 4: Final commit + summary**

```bash
git add README.md
git commit -m "Document expected upstream-merge conflict surface" --allow-empty
```

(Use `--allow-empty` if you skipped Step 3 — to anchor the end of the rebrand series.)

---

# Done

After Task 38 the rebrand branch `rebrand/spk-editor` is ready for review and merge into `main`. Suggested final action — **not a task**, decided by the user: open a PR (or fast-forward `main` if working solo), then unstash the `codebook.toml` change with `git stash pop`.
