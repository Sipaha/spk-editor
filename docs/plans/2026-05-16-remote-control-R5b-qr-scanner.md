# R-5b: Android QR scanner — pair from a scanned `spk-remote://` URL

**Status:** planned (awaiting Android SDK install on the maintainer's machine)
**Repo:** `spk-editor-android-client/` (sibling of `spk-editor`)
**Depends on:** R-5a (`:core` parsing + connection layer), Android SDK present (`ANDROID_HOME` set, platform-34 + build-tools-34.x).
**Goal:** Replace the R-5a "paste URL" Compose surface with a real QR scanner. Scanning the QR shown by the spk-editor Remote Control modal (server fingerprint + secret + name embedded) parses straight into `PairingUrl` and transitions to `Connecting`.

## Why this phase exists

R-5a's `:app` accepts the pairing URL via a `TextField`, which is fine for dev smoke but not the actual UX. The pairing flow on the server side (R-3) generates a QR encoding `spk-remote://<host>:<port>?secret=<base64>&client=<name>&fp=<hex>`. Mobile-only consumers should scan, not type — typing 32-byte base64 secrets on a phone keyboard is infeasible.

## Scope

### Library choice: zxing-android-embedded vs CameraX + ML Kit

Pick **zxing-android-embedded** (`com.journeyapps:zxing-android-embedded:4.3.0`):
- Plug-and-play `ScanContract` + `ScanOptions` — about 10 lines of Compose code to integrate.
- Hardcoded camera handling, permissions wrapper.
- AGPL 3.0 — compatible with this repo's GPL 3.0-or-later.

CameraX + ML Kit gives finer control but needs ~3× more code for a feature that's identical from the user POV. Drop ML Kit since the only target is QR codes.

### Files

```
app/src/main/kotlin/ru/sipaha/spkremote/app/
  qr/
    QrPairingScreen.kt    # Compose screen with Scan button + result handling
    QrScanContract.kt     # Wrapper around ScanContract<ScanOptions, ScanIntentResult>
  ui/App.kt               # nav: Disconnected → QrPairing (replaces direct URL input)
  ui/UrlInputFallback.kt  # OPTIONAL: keep the URL TextField as "Enter manually" link from QrPairingScreen for dev/debug
  vm/MainViewModel.kt     # add fun pairFromScannedUrl(raw: String): Boolean
```

### Behavior

1. App opens to `QrPairingScreen` (replacing direct paste). UI: app logo, big "Scan pairing QR" button, small "Enter manually" link.
2. Tapping "Scan" launches the zxing scanner activity. App requests CAMERA permission inline (the contract handles the rationale rendering).
3. On scan success, `pairFromScannedUrl(raw)`:
   - `PairingUrl.parse(raw)` (already from R-5a). On failure: snackbar "Not a valid SPK Editor pairing QR".
   - On success: transitions ViewModel state to `Connecting`, runs `RemoteClient.connect()`, transitions to `Connected(caps)` or `Error(msg)`.
4. Manually-entered URL takes the same code path.

### Permissions

- `CAMERA` runtime permission. Rationale: "Needed to scan the pairing QR shown by SPK Editor on your computer."
- No location, no network beyond what the WS connect uses (no new permissions there).

### Out of scope

- Persisting last-paired server. Defer to R-5c when there's a multi-server view.
- Front-camera QR scanning (zxing handles this; don't expose unless asked).
- Importing a pairing URL via Android's share-intent ("send pairing URL to phone"). Nice-to-have, defer.

## Architectural decisions

1. **zxing over CameraX**. Smaller surface, AGPL-compatible.
2. **`QrPairingScreen` is the new launch destination**, replacing R-5a's "paste URL" stub. The text field becomes a fallback behind a smaller link, not the primary affordance.
3. **Permission handling lives in the scanner contract**, not the ViewModel. The activity-result wrapper is the cleanest place for runtime permission, and the ViewModel stays Android-framework-free where possible.

## Verification

```bash
cd /home/spk/.spk/spk-editor/solutions/spk-solutions/spk-editor-android-client
JAVA_HOME=$HOME/.jdks/temurin-21.0.10 ./gradlew :app:assembleDebug 2>&1 | tee /tmp/r5b.txt
grep -E "BUILD SUCCESSFUL|FAILURE:" /tmp/r5b.txt
```

Manual smoke (sub-agent or maintainer with a device):
- Install `:app` debug APK on an Android device.
- Boot spk-editor with Remote Control enabled, generate a client, show its QR.
- On the phone: scan → see "Connecting" → "Connected" with the protocol version.

## Acceptance

- [ ] `:app:assembleDebug` BUILD SUCCESSFUL.
- [ ] Manual smoke: scanning a real R-3 QR results in a connected state.
- [ ] Camera-permission-denied path shows a snackbar, no crash.
- [ ] Malformed QR (random URL or non-`spk-remote://` scheme) shows "Not a valid SPK Editor pairing QR" snackbar, no crash.
- [ ] "Enter manually" fallback still works (regression check on R-5a behavior).

## When done

Sub-agent reports the commit SHA, the zxing version used, whether the permission flow surfaced any AGP / target-SDK 34 quirk, and which device/emulator was used for manual smoke.

## Notes for the next phase

R-5c picks up after this lands with the connected-state surface: solutions list, then sessions list, then chat. R-5b leaves the "Connected" screen as a placeholder showing only the protocol version — that gets replaced wholesale by R-5c's navigation graph.
