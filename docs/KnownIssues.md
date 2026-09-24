# Known issues

Confirmed defects that are **not fixed yet**. Each entry records what breaks, the evidence in the
tree, what a fix would touch, and why it was left alone. Entries marked *Windows-visible fix* were
found while auditing Linux support: the fix would change behavior on Windows too, so it is a
deliberate product decision rather than a platform fix.

Nothing here is speculative: every entry was reproduced by reading the listed code, and where a
runtime check was possible it was performed (see the evidence line).

## Summary

| # | Platform | Area | Symptom | Fix would touch |
|---|---|---|---|---|
| 1 | all | Settings → VR Overlay | Controls stay interactive when the runtime status call fails, so Linux users can toggle an overlay that is always `unsupported` | Windows error path |
| 2 | Linux | Window shell | A failed `app_build_info` call leaves the undecorated window with no resize handles and no window-manager border | Windows shell code shares the component |
| 3 | all | Settings → Software updates | Status line reads "Updates are not available for this build." for the moment before build info loads | Windows display |
| 4 | all | Settings tab bar | `Debug` is the only category label not routed through i18n | Windows display |
| 5 | Linux | Credential storage | **Fixed**: writes are serialized by an advisory lock | — |
| 6 | Linux | Per-process capture | A time-warped capture was reported once; not reproducible with a Core-like harness | Needs a check with real VRChat under Proton |
| 7 | all | Translation prompt | The Norwegian target-language label reaches the model as mojibake (`Norwegian Bokm姘搇`) | 1-line string fix plus a table-consistency test |
| 8 | Linux | Capture, *System default* | The default-mode stream still pins `target.object` to the default at start; whether WirePlumber 0.5 then follows a default change is unverified | `audio/linux/capture.rs` `prepare` |
| 9 | all | Per-process capture | After VRChat restarts (new pid) the capture keeps waiting on the old pid and stays silent until it is restarted | Both backends |
| 10 | Linux | Desktop shell | `xdg-open` children (open logs folder, open VRCX-0 page) are never reaped and stay as zombies until VRCS exits | `diagnostics.rs`, `lib.rs` |

## 1. VR Overlay gate fails open when the status call fails

- **Code**: `apps/desktop/src/settings/sections/VrOverlaySettingsSection.tsx:185-196`
- **What breaks**: `runtimeState` defaults to `"initializing"` and the gate is
  `overlayUnsupported = runtimeState === "unsupported"`. When `getVrOverlayStatus()` rejects, the
  catch only sets `statusError` and leaves `status` null, so `overlayUnsupported` stays false and
  the Enable switch, retry button and sample controls stay interactive — on Linux, where the
  runtime reports `unsupported` from the very first read
  (`apps/desktop/src-tauri/src/vr_overlay/runtime.rs:100-106`, kept in place by `tick` at
  `:409-410`), the error banner and the enabled controls contradict each other.
- **Evidence**: the Rust command exists on every platform and returns `unsupported` on Linux, so
  this only triggers on an IPC failure; the code comment calls the gate "hard", which it is not.
- **Suggested fix**: `const overlayUnsupported = Boolean(statusError) || runtimeState === "unsupported";`
- **Why it was not fixed**: that also disables the overlay controls on Windows when the status call
  fails, i.e. it changes non-Linux behavior. *Windows-visible fix.*
- **Verification**: with the fix, force `getVrOverlayStatus()` to reject and assert the Enable
  switch is disabled.

## 2. Linux resize handles fail open when build info cannot be loaded

- **Code**: `apps/desktop/src/shell/WindowResizeHandles.tsx:37-47`
- **What breaks**: the component fetches `app_build_info` itself and renders the resize strips only
  when `platform === "linux" && !maximized`. A rejected promise leaves `platform` null, so a
  frameless window (`decorations: false` in `apps/desktop/src-tauri/tauri.conf.json:20`) ends up
  with neither custom handles nor a window-manager border: the window cannot be resized.
- **Evidence**: `app_build_info` is a synchronous Tauri command, so a failure is unlikely; the
  failure mode, however, is a hard usability loss with no recovery other than restarting.
- **Suggested fix**: pass the already-loaded build info down from `useAppUpdater` (through
  `WindowChrome`) instead of fetching it a second time, or keep the last known platform on error
  rather than treating an error as "not Linux".
- **Why it was not fixed**: the component is shared with Windows/macOS window chrome, so the change
  is not Linux-scoped. *Windows-visible fix.*
- **Verification**: stub `loadAppBuildInfo` to reject and assert the handles are still rendered on
  Linux.

## 3. Update status flashes "unavailable" before build info loads

- **Code**: `apps/desktop/src/updates/SoftwareUpdateSettings.tsx:7-8`
- **What breaks**: `if (!updater.buildInfo?.updaterAvailable) return "updates.status.unavailable";`
  treats a `null` build info (still loading, or a rejected load whose error `useAppUpdater`
  swallows) as "this build has no updater", so the panel briefly claims updates are unavailable on
  Windows too.
- **Evidence**: `buildInfo` starts as `null` in `useAppUpdater` and is filled asynchronously.
- **Suggested fix**: only report unavailable once it is known —
  `updater.buildInfo && !updater.buildInfo.updaterAvailable ? "updates.status.unavailable" : …`
- **Related**: the "Check for updates automatically" switch was changed to use exactly that
  "known-unavailable" rule, because on Linux the updater is compiled out
  (`apps/desktop/src-tauri/src/app_updates.rs:28-30`) and the switch could otherwise persist a
  preference that never takes effect.
- **Why it was not fixed**: it changes what Windows shows during startup. *Windows-visible fix.*
- **Verification**: render the panel with `buildInfo: null` and assert the status is not the
  "unavailable" string.

## 4. `Debug` settings category label is hardcoded

- **Code**: `apps/desktop/src/settings/components/SettingsTabBar.tsx:36`
- **What breaks**: `label: "Debug"` is the only category label not passed through `t(...)`, so it
  stays English in every locale. `scripts/check-i18n.mjs` only compares locale files, so it cannot
  catch this.
- **Suggested fix**: add a `settings.categories.debug` key to all four locales and use `t(...)`.
- **Why it was not fixed**: not introduced by the Linux work and not Linux-specific.

## 5. Linux credential store has no cross-process lock — fixed

- **Code**: `core/src/credentials.rs` (`with_store_lock`, `#[cfg(not(windows))]`)
- **Was**: every write was an unguarded read-modify-write of the whole JSON file, so two processes
  sharing `$XDG_DATA_HOME/vrcs/credentials.json` could lose one of two concurrent keys. A test with
  8 writers x 25 distinct keys, each writer on its own file handles, kept only 24 of 200 keys.
- **Now**: writes and deletes hold an exclusive `flock` on the sibling `credentials.lock` (0600) for
  the read-modify-write; reads stay lock-free. The same test keeps 200/200.
- **Still open**: `config.json` has the same shape of problem when two Cores share one configuration
  file; that is a product decision (single writer vs. locking), not a platform fix.

## 6. Per-process capture can deliver time-warped audio — not reproducible

- **Documented in**: `docs/Linux.md`, "Per-process capture" (`Status: experimental`).
- **What was checked** (Ubuntu 24.04, PipeWire 1.0.5, WirePlumber 0.4.17): a throwaway in-crate probe
  drove `AudioCapture` exactly like the Core — `start(None, Some("VRChat.exe"))` inside
  `spawn_blocking` on a multi-thread runtime, with `list_devices()` polled every 500 ms alongside —
  against a player process whose `comm` is `VRChat.exe`. Native (`pw-cat`) and PulseAudio-protocol
  (`pacat`, how Wine plays audio) players, 48 kHz and 44.1 kHz streams on a 48 kHz graph: the 440 Hz
  tone arrived at 440 Hz with the played level and ~16 000 frames/s, and synthesized speech tapped
  the same way was accepted by the Silero VAD (324 of 370 chunks flagged as speech, two segments).
- **One artifact worth knowing**: `pacat`/`paplay` choose their mode from `argv[0]`. A copy renamed to
  `VRChat.exe` plays a WAV as *raw* 44.1 kHz data unless `--file-format=wav` is given, which shifts a
  440 Hz tone to ~404 Hz — at the sink monitor too, so before VRCS sees it. A harness built that way
  reproduces exactly the "right level and rate, no peak where expected" symptom described earlier.
  Whether that explains the original report is not known.
- **Next step**: one run with the real VRChat under Proton using the end-to-end recipe in
  `docs/Linux.md`; if it is clean, the *experimental* label can go.

## 7. The Norwegian target-language label is corrupted in the LLM prompt

- **Code**: `core/src/providers.rs:1165` — `"nb" => "Norwegian Bokm姘搇",`
- **What breaks**: the value is UTF-8 mojibake (the bytes of `Norwegian Bokmål` were decoded and
  re-encoded wrong). `translation_language_name` is used to build the target-language line of every
  LLM translation request (`core/src/translation/prompt.rs:63-65`), so translating into Norwegian
  sends the model `Target language: Norwegian Bokm姘搇 [nb]`. The UI shows the correct name, because
  the frontend keeps its own table (`apps/desktop/src/translation-languages.ts:26`), so nothing in
  the app reveals the corruption.
- **Evidence**: `core/src/providers.rs:1165` is the only non-ASCII string in that file, and the
  code list it belongs to (`LLM_TRANSLATION_LANGUAGES`, `:79-83`) contains `nb`. The two tables
  currently hold the same 34 codes (`DEEPL_TRANSLATION_LANGUAGES`, `:85-89`, is a subset missing
  `fil`, `ms`, `yue-Hant`, `hi`), so this is a corrupted value rather than a missing entry.
- **Suggested fix**: restore `"Norwegian Bokmål"`, and add a test that walks
  `LLM_TRANSLATION_LANGUAGES` asserting `translation_language_name` returns `Some` for every code,
  that `DEEPL_TRANSLATION_LANGUAGES` stays a subset, and that every name is ASCII. Nothing covers
  `translation_language_name` today (`core/src/providers.rs:1356-1372` exercises only
  `is_valid_translation_language` and `supports_translation_language`), which is why the corruption
  survived.
- **Why it was not fixed**: it was found while reviewing the provider catalog, not the Linux port,
  and was left for the owner of that change to confirm the intended spelling.
- **Verification**: `cargo test --manifest-path core/Cargo.toml --lib providers` with the
  consistency test above.

## 8. *System default* capture pins the current default node

- **Code**: `core/src/audio/linux/capture.rs` (`prepare`) and `devices::resolve_target`
- **What happens**: with no device selected, `resolve_target` still resolves the current default
  node and the stream carries it as `target.object`. With WirePlumber 0.4.17 the stream follows a
  later default change anyway (verified with `wpctl set-default` mid-capture: the capture moved to
  the new default's monitor), which matches WASAPI's `follows_default`. WirePlumber 0.5 treats
  `target.object` as a defined target, so there the capture may stay on the old default.
- **Suggested fix**: leave `target.object` unset in default mode and let the session manager route
  the `stream.capture.sink` stream to the default.
- **Why it was not fixed**: not reproducible on this machine (no WirePlumber 0.5), and the current
  behaviour is correct where it could be tested.

## 9. Per-process capture does not follow a restarted VRChat

- **Code**: `core/src/audio.rs` `AudioCapture::start` resolves the pid once; both
  `audio/linux/capture.rs` (tap by pid) and `audio/wasapi/capture.rs` (process loopback by pid)
  keep that pid for the whole session.
- **What breaks**: when VRChat exits and starts again, the new process is never tapped; capture keeps
  running and produces no audio until the user stops and starts it.
- **Why it was not fixed**: same behaviour on Windows, so it is a product change for both backends.

## 10. `xdg-open` children are not reaped

- **Code**: `apps/desktop/src-tauri/src/diagnostics.rs` `open_directory`,
  `apps/desktop/src-tauri/src/lib.rs` `open_vrcx_repository`
- **What happens**: `Command::spawn` without `wait`; unless something else reaps the child, each
  click leaves a `<defunct>` process until VRCS exits. Found by reading the code, not observed at
  runtime. Harmless in practice, but visible in `ps`.
- **Suggested fix**: reap in a background thread, or use `tauri-plugin-opener`.

