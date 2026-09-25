import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import i18next from "i18next";

import { platformContext } from "../src/i18n/platform-context.ts";

const WEBKITGTK =
  "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15";
const WEBVIEW2 =
  "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36 Edg/140.0.0.0";
const ANDROID =
  "Mozilla/5.0 (Linux; Android 14) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Mobile Safari/537.36";

test("only a Linux webview selects the Linux message variants", () => {
  assert.equal(platformContext(WEBKITGTK), "linux");
  assert.equal(platformContext(WEBVIEW2), undefined);
  assert.equal(platformContext(ANDROID), undefined);
  assert.equal(platformContext(""), undefined);
});

test("audio errors keep the Windows remediation and fall back without a Linux variant", async () => {
  const english = JSON.parse(
    readFileSync(new URL("../src/i18n/locales/en-US.json", import.meta.url), "utf8"),
  ) as { translation: Record<string, unknown> };
  const i18n = i18next.createInstance();
  await i18n.init({
    resources: { "en-US": { translation: english.translation } },
    lng: "en-US",
    interpolation: { escapeValue: false },
  });

  const windows = i18n.t("errors.audio.process_loopback_unavailable", {
    context: platformContext(WEBVIEW2),
  });
  const linux = i18n.t("errors.audio.process_loopback_unavailable", {
    context: platformContext(WEBKITGTK),
  });
  assert.match(windows, /Windows 11/);
  assert.doesNotMatch(linux, /Windows/);
  // A code without a Linux variant falls back to its only message.
  assert.equal(
    i18n.t("errors.audio.start_timeout", { context: "linux" }),
    i18n.t("errors.audio.start_timeout"),
  );
});

test("platform wording resolves to the Windows text by default and the Linux text on Linux", async () => {
  const english = JSON.parse(
    readFileSync(new URL("../src/i18n/locales/en-US.json", import.meta.url), "utf8"),
  ) as { translation: Record<string, unknown> };
  const i18n = i18next.createInstance();
  await i18n.init({
    resources: { "en-US": { translation: english.translation } },
    lng: "en-US",
    interpolation: { escapeValue: false },
  });

  for (const key of [
    "updates.status.unavailable",
    "settings.apiManagement.securityNotice",
    "settings.audio.systemOutputDescription",
    "settings.audio.defaultMicrophoneDescription",
    "onboarding.recognition.addApiDescription",
    "errors.desktop.autostart_not_applied",
  ]) {
    const windows = i18n.t(key, { context: platformContext(WEBVIEW2) });
    const linux = i18n.t(key, { context: platformContext(WEBKITGTK) });
    assert.notEqual(windows, linux, key);
    assert.equal(linux, i18n.t(`${key}_linux`), key);
  }
});

test("every Linux variant has a base key to fall back to in every locale", () => {
  const localeDirectory = new URL("../src/i18n/locales/", import.meta.url);
  for (const name of ["en-US", "ja-JP", "zh-CN", "zh-Hant"]) {
    const resource = JSON.parse(
      readFileSync(new URL(`${name}.json`, localeDirectory), "utf8"),
    ) as { translation: Record<string, unknown> };
    const variants: string[] = [];
    const visit = (node: Record<string, unknown>, path: string) => {
      for (const [key, value] of Object.entries(node)) {
        const full = path ? `${path}.${key}` : key;
        if (value && typeof value === "object") visit(value as Record<string, unknown>, full);
        else if (key.endsWith("_linux")) {
          variants.push(full);
          assert.equal(typeof node[key.slice(0, -"_linux".length)], "string", `${name}: ${full}`);
        }
      }
    };
    visit(resource.translation, "");
    assert.ok(variants.length >= 12, `${name}: expected the Linux variants, found ${variants.length}`);
    // The PipeWire build never reports an unsupported platform.
    assert.ok(!variants.includes("errors.audio.unsupported_platform_linux"), name);
  }
});
