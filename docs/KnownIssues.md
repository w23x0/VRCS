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
| 5 | Linux | Credential storage | Two processes writing credentials at once can lose one of the two keys | Linux-only fix, but needs a locking design |
| 6 | Linux | Per-process capture | Captured application audio has been observed time-warped (already documented in `docs/Linux.md`) | Unverified cause |
| 7 | all | Translation prompt | The Norwegian target-language label reaches the model as mojibake (`Norwegian Bokm姘搇`) | 1-line string fix plus a table-consistency test |

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

## 5. Linux credential store has no cross-process lock

- **Code**: `core/src/credentials.rs` (`read_store` / `write_store`, `#[cfg(not(windows))]`)
- **What breaks**: every write is read-modify-write of the whole JSON file. Two processes that
  share `$XDG_DATA_HOME/vrcs/credentials.json` (for example the desktop shell with its in-process
  Core plus a standalone Core started for frontend development, or two shells) can interleave and
  lose one of the two keys.
- **Evidence**: within one process the writes are already serialized — every credential write path
  holds `config_control` (`core/src/server/cloud.rs:252,303,347,395,418,451`,
  `core/src/server/external.rs:44`, `core/src/server/vrcx.rs:35`). The atomic rename already
  guarantees readers never see a half-written file; what can be lost is a concurrent key.
- **Suggested fix**: take an advisory lock (`flock`) on a sibling lock file around the
  read-modify-write, or make the Core the only writer.
- **Why it was not fixed**: it needs a locking design decision (and the same question applies to
  `config.json`), so it is out of scope for a review of the Linux port.

## 6. Per-process capture can deliver time-warped audio

- **Documented in**: `docs/Linux.md`, "Per-process capture" (`Status: experimental`).
- **State**: observed through the Core API, not reproduced on this machine — the two PipeWire tap
  tests assert a 440 Hz tone peak measured at 16 kHz
  (`core/src/audio/linux/mod.rs:442-484`) and pass against a live PipeWire session, and a probe of
  the negotiated format shows `16000 Hz / 1 channel / F32LE` on all four capture paths (sink
  monitor, microphone, `pw-play` tap, Pulse-protocol tap). Whatever produces the time warp is
  therefore outside the paths the tests exercise; it has not been observed with the test harness.
- **Next step**: reproduce with VRChat under Wine/Proton using the end-to-end recipe in
  `docs/Linux.md`, then compare the negotiated format and the buffer pacing of the tapped stream
  against the test harness.

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
