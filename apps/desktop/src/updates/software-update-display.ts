/** Fields of {@link AppUpdaterState} that decide how the automatic-checks switch is drawn. */
export interface AutomaticChecksToggleInput {
  /** `null` while the build info is still loading (and when the command failed). */
  buildInfo: { updaterAvailable: boolean } | null;
  automaticChecks: boolean;
  preferenceReady: boolean;
  preferenceSaving: boolean;
}

export interface AutomaticChecksToggleState {
  /** What the switch renders as. Never the stored preference once no updater exists. */
  checked: boolean;
  disabled: boolean;
}

/** True only once the build info has answered "there is no updater" (always so on Linux). */
function updaterUnavailable(buildInfo: { updaterAvailable: boolean } | null): boolean {
  return buildInfo?.updaterAvailable === false;
}

/**
 * Display state of the "check for updates automatically" switch.
 *
 * When the build carries no updater the switch is shown off even if the stored
 * preference says on: a greyed switch reading "on" tells the user a check is going to
 * happen when none can. Only the rendering changes — the stored preference is left
 * alone, so a build that does have an updater shows the preference again.
 *
 * A missing build info means "unknown", not "unavailable", so the switch keeps the
 * preference until the answer arrives instead of flashing off on every launch.
 *
 * The `checked` override is safe only because `disabled` is always set with it: the
 * toggle writes `!checked`, so an enabled switch shown off would store "on" when the
 * user meant to turn it on from a stored "on".
 */
export function automaticChecksToggleState(
  updater: AutomaticChecksToggleInput,
): AutomaticChecksToggleState {
  const unavailable = updaterUnavailable(updater.buildInfo);
  return {
    checked: unavailable ? false : updater.automaticChecks,
    disabled: !updater.preferenceReady || updater.preferenceSaving || unavailable,
  };
}