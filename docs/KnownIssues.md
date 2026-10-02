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
| 6 | Linux | Per-process capture | A time-warped capture was reported once; not reproducible with a Core-like harness, and the *experimental* label was removed by owner decision (2026-09-30) | Still unverified with real VRChat under Proton; kept for observation |
| 7 | all | Translation prompt | The Norwegian target-language label reaches the model as mojibake (`Norwegian Bokm姘搇`) | 1-line string fix plus a table-consistency test |
| 8 | Linux | Capture, *System default* | **Not a defect**: the default-mode stream does pin `target.object`, but WirePlumber 1.6.2 still moves it to the new default (verified for sinks and microphones, in both directions) | — |
| 9 | Linux | Per-process capture | **Fixed**: the capture re-resolves the pid by process name when the target process exits, and moves the tap to the restarted application | Windows still binds to the pid, see below |
| 10 | Linux | Desktop shell | **Fixed**: launched helper processes are reaped by a background thread | — |

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
- **`config.json`**: has the same shape of problem when two Cores share one configuration file. Decided:
  the Core stays the single writer of its configuration and no lock is added; `docs/Linux.md` states
  that a standalone Core must not share the desktop app's configuration file.

## 6. Per-process capture can deliver time-warped audio — not reproducible

- **Documented in**: `docs/Linux.md`, "Per-process capture" (no longer marked experimental).
- **What was checked** (Ubuntu 24.04, PipeWire 1.0.5, WirePlumber 0.4.17): a throwaway in-crate probe
  drove `AudioCapture` exactly like the Core — `start(None, Some("VRChat.exe"))` inside
  `spawn_blocking` on a multi-thread runtime, with `list_devices()` polled every 500 ms alongside —
  against a player process whose `comm` is `VRChat.exe`. Native (`pw-cat`) and PulseAudio-protocol
  (`pacat`, how Wine plays audio) players, 48 kHz and 44.1 kHz streams on a 48 kHz graph: the 440 Hz
  tone arrived at 440 Hz with the played level and ~16 000 frames/s, and synthesized speech tapped
  the same way was accepted by the Silero VAD (324 of 370 chunks flagged as speech, two segments).
- **Re-checked through the Core API** (Ubuntu 26.04, PipeWire 1.6.2, WirePlumber 1.6.2): one
  combination this round — a PulseAudio-protocol client playing a 48 kHz stereo 440 Hz test tone —
  captured 440.00 Hz at level 0.4 and ~16 000 frames/s.
- **One artifact worth knowing**: `pacat`/`paplay` choose their mode from `argv[0]`. A copy renamed to
  `VRChat.exe` plays a WAV as *raw* 44.1 kHz data unless `--file-format=wav` is given, which shifts a
  440 Hz tone to ~404 Hz — at the sink monitor too, so before VRCS sees it. A harness built that way
  reproduces exactly the "right level and rate, no peak where expected" symptom described earlier.
  Whether that explains the original report is not known.
- **Owner decision (2026-09-30)**: the *experimental* label was removed without that run. The entry
  stays open for observation, because the report has still not been reproduced and the mode has
  still not been exercised against the real VRChat under Proton. **Next step**: one run with the real
  VRChat under Proton using the end-to-end recipe in `docs/Linux.md`; if it is clean, this entry can
  be closed.

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

## 8. *System default* capture pins the current default node — not a defect on WirePlumber 1.6.2

- **Code**: `core/src/audio/linux/capture.rs` (`prepare`) and `devices::resolve_target`
- **What happens**: with no device selected, `resolve_target` still resolves the current default
  node and the stream carries it as `target.object`. The worry was that WirePlumber 0.5+ treats
  `target.object` as a defined target and would therefore keep the capture on the old default
  instead of following the change.
- **Verified on WirePlumber 1.6.2** (PipeWire 1.6.2, real `wpctl set-default` during a capture
  driven through `AudioCapture`, two null sinks playing 440 Hz and 220 Hz): the session manager
  moves the stream to the new default anyway, so *System default* follows, matching WASAPI's
  `follows_default`. The capture followed in both directions — 440 Hz → 220 Hz when the default
  moved from `vrcs-d9-a` to `vrcs-d9-b`, and 220 Hz → 440 Hz when it moved back — and the link
  moved with it (`vrcs-d9-a:monitor_FL |-> vrcs-capture:input_MONO` became
  `vrcs-d9-b:monitor_FL |-> vrcs-capture:input_MONO`). The microphone direction behaves the same
  way: with two virtual sources the capture followed `vrcs-d9-mic-a:capture_FL` to
  `vrcs-d9-mic-b:capture_FL`. Explicitly selected devices did **not** follow in either direction,
  which is the `node.dont-reconnect` fix from 3.2 in the review report.
- **What the stream actually carries** (`pw-dump` on the live `vrcs-capture` node during
  default-mode capture): `media.class = "Stream/Input/Audio"`, `stream.capture.sink = true`,
  `target.object = "vrcs-d9-a"` and **no** `node.dont-reconnect`. The pin is real, and
  WirePlumber 1.6.2 overrides it anyway — the microphone stream carries the same shape minus
  `stream.capture.sink`.
- **Why nothing was changed**: the behaviour is already correct on both WirePlumber generations
  that have been tested, so leaving `target.object` unset in default mode would be a speculative
  change. The entry stays because the pin is still in the code and a future session manager could
  change that.
- **Re-verification recipe** (not an automated test: it moves the system default device, which a
  committed test must never do):

  ```bash
  # two low-priority null sinks so they cannot take the default on their own
  pw-cli -m create-node adapter '{ factory.name=support.null-audio-sink node.name=d9-a \
    node.description=d9-a media.class=Audio/Sink audio.position=[FL,FR] \
    priority.session=1 priority.driver=1 }' &
  pw-cli -m create-node adapter '{ factory.name=support.null-audio-sink node.name=d9-b \
    node.description=d9-b media.class=Audio/Sink audio.position=[FL,FR] \
    priority.session=1 priority.driver=1 }' &

  # node.dont-reconnect keeps each player on its own sink (review report 3.1)
  pw-play --target d9-a --properties '{ node.dont-reconnect = true }' a440.wav &
  pw-play --target d9-b --properties '{ node.dont-reconnect = true }' b220.wav &

  wpctl status          # the Sinks list prints each node's id: use d9-a's to start there
  wpctl set-default <d9-a id>
  # start the capture (explicit device = d9-a, or no device for *System default*),
  # then mid-capture:  wpctl set-default <d9-b id>
  # watch the dominant frequency and:  pw-link -l | grep -B1 vrcs-capture
  ```

  Expect *System default* to switch to 220 Hz and its link to `d9-b:monitor_FL`, and an explicit
  `d9-a` to stay at 440 Hz on `d9-a:monitor_FL`. Restore the original default afterwards
  (`wpctl set-default <original id>`, and check `pw-metadata -n default 0` — on WirePlumber 0.5+
  `wpctl set-default` also rewrites `default.configured.audio.sink`).

## 9. Per-process capture does not follow a restarted VRChat — fixed on Linux

- **Code**: `core/src/audio.rs` `AudioCapture::start` resolves the pid once; `audio/linux/capture.rs`
  (tap by pid) and `audio/wasapi/capture.rs` (process loopback by pid) both kept that pid for the
  whole session.
- **What broke**: when VRChat exits and starts again, the new process is never tapped; capture kept
  running and produced no audio until the user stopped and started it.
- **Now (Linux)**: the process name travels with the capture target
  (`CaptureTarget::process(pid, name)`, `core/src/audio/linux/mod.rs`). Every ~250 ms refresh
  (`audio/linux/capture.rs`, `follow_target_process`) checks that the pid is still the target
  process — `/proc/<pid>` gone, or the pid reused by something else, both count as gone — and, at
  most once a second, resolves the name again. A new pid is adopted, the old streams' tap proxies
  are dropped when they disappear from the graph, and a `tracing::info!` line records the switch.
  While the old process is alive nothing changes. While no new process exists the capture stays
  silent rather than failing or falling back to whole-system audio.
- **Windows is unchanged**: the WASAPI backend binds its loopback client to the pid resolved at
  start (`audio/wasapi/capture.rs`, `CaptureTarget::Process(process_id)`), and its
  `CaptureTarget::process` deliberately drops the name — that path is untouched, so a restarted
  VRChat still needs a manual restart of capture on Windows. Changing it is a product decision for
  that backend, not a Linux port fix.
- **Verification**:
  - `audio::linux::devices::tests::the_captured_process_is_tracked_until_it_exits_or_gets_reused`
    (process still alive / exited / pid reused / pid reused back by the same name) and
    `a_restarted_process_is_found_under_its_new_pid` against a synthetic procfs tree;
  - `audio::linux::capture::tests::a_live_target_is_never_looked_up_again` and
    `a_missing_target_is_relooked_up_throttled_and_adopts_a_new_pid` for the throttle and the switch;
  - `audio::linux::tests::process_capture_follows_a_restarted_process` (PipeWire integration): a
    player named `VRChat.exe` plays 440 Hz, the process is killed by pid, a second player with the
    same name plays 220 Hz, and the running capture picks up 220 Hz without being restarted —
    measured at ~1 s, which is the re-lookup throttle. With the follow logic stubbed out the same
    test captures 0 frames, so it is not vacuous.

## 10. `xdg-open` children are not reaped — fixed

- **Code**: `apps/desktop/src-tauri/src/diagnostics.rs` `open_directory`,
  `apps/desktop/src-tauri/src/lib.rs` `open_vrcx_repository`
- **Was**: `Command::spawn` without `wait`; unless something else reaps the child, each click left
  a `<defunct>` process until VRCS exits. Found by reading the code, not observed at runtime.
  Harmless in practice, but visible in `ps`.
- **Now**: both call sites go through one helper, `reaper::spawn_detached`
  (`apps/desktop/src-tauri/src/reaper.rs`), which spawns the command and hands the `Child` to a
  named background thread (`vrcs-helper-reaper`) that `wait`s on it; a failed `wait` is only
  logged with `tracing::debug!`. No new dependency — `tauri-plugin-opener` was not needed, and
  adding it would have been the more Windows-visible change.
- **Windows**: the helper is shared, because the command construction already was. On Windows this
  closes the `explorer.exe` handle from a background thread instead of dropping it inline; nothing
  is read from the child, the exit status was never used, and the UI thread still does not block,
  so the user-visible behaviour is unchanged.
- **Verification**: `reaper::tests` (Linux-only) covers both directions —
  `child_spawned_without_waiting_stays_a_zombie` spawns `true` and drops the `Child`, and polls
  `/proc/<pid>/stat` until the state is `Z`; `detached_helper_is_reaped` spawns `true` through the
  helper and polls until the pid is gone from `/proc` entirely. With the reaping thread removed the
  second test fails with `last state Some('Z')`, so it is not vacuous.

