import assert from "node:assert/strict";
import test from "node:test";

import {
  automaticChecksToggleState,
  type AutomaticChecksToggleInput,
} from "../src/updates/software-update-display.ts";

function updater(
  overrides: Partial<AutomaticChecksToggleInput> & Pick<AutomaticChecksToggleInput, "buildInfo">,
): AutomaticChecksToggleInput {
  return { automaticChecks: true, preferenceReady: true, preferenceSaving: false, ...overrides };
}

const withoutUpdater = { updaterAvailable: false };
const withUpdater = { updaterAvailable: true };

test("an unloaded build info keeps the stored preference and only waits for it", () => {
  const on = automaticChecksToggleState(updater({ buildInfo: null, preferenceReady: true }));
  assert.equal(on.checked, true);
  assert.equal(on.disabled, false);

  const off = automaticChecksToggleState(updater({ buildInfo: null, automaticChecks: false }));
  assert.equal(off.checked, false);
  assert.equal(off.disabled, false);

  // The stored preference has not arrived yet, so the switch waits and is not shown off.
  const loading = automaticChecksToggleState(updater({ buildInfo: null, preferenceReady: false }));
  assert.equal(loading.checked, true);
  assert.equal(loading.disabled, true);
});

test("a build without an updater shows the switch off even when the preference is on", () => {
  const state = automaticChecksToggleState(updater({ buildInfo: withoutUpdater }));
  assert.equal(state.checked, false);
  assert.equal(state.disabled, true);
});

test("a build with an updater shows the stored preference", () => {
  assert.deepEqual(
    automaticChecksToggleState(updater({ buildInfo: withUpdater, automaticChecks: true })),
    { checked: true, disabled: false },
  );
  assert.deepEqual(
    automaticChecksToggleState(updater({ buildInfo: withUpdater, automaticChecks: false })),
    { checked: false, disabled: false },
  );
});

test("the preference being saved or unread disables the switch on every platform", () => {
  const saving = automaticChecksToggleState(
    updater({ buildInfo: withUpdater, preferenceSaving: true }),
  );
  assert.equal(saving.disabled, true);
  const unread = automaticChecksToggleState(
    updater({ buildInfo: withUpdater, preferenceReady: false }),
  );
  assert.equal(unread.disabled, true);
  assert.equal(unread.checked, true);
});