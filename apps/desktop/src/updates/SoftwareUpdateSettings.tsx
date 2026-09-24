import { RefreshCw } from "lucide-react";
import { useTranslation } from "react-i18next";

import { PreferenceToggle } from "../settings/SettingsControls";
import { platformContext } from "../i18n/platform-context";
import type { AppUpdaterState } from "./useAppUpdater";

function updateStatusKey(updater: AppUpdaterState): string {
  if (!updater.buildInfo?.updaterAvailable) return "updates.status.unavailable";
  if (updater.phase === "checking") return "updates.status.checking";
  if (updater.phase === "upToDate") return "updates.status.upToDate";
  if (updater.phase === "available") return "updates.status.available";
  if (updater.phase === "downloading") return "updates.status.downloading";
  if (updater.phase === "installing") return "updates.status.installing";
  if (updater.phase === "error") return `updates.errors.${updater.errorCode ?? "failed"}`;
  return "updates.status.ready";
}

export function SoftwareUpdateSettings({ updater }: { updater: AppUpdaterState }) {
  const { t } = useTranslation();
  const busy = updater.phase === "checking"
    || updater.phase === "downloading"
    || updater.phase === "installing";
  const statusKey = updateStatusKey(updater);
  // 只在构建信息已明确"没有更新器"时锁住开关（Linux 恒为 true）：未知不等于不可用，
  // 否则每次启动都会在读取构建信息前闪一下灰。
  const updaterUnavailable = updater.buildInfo?.updaterAvailable === false;

  return (
    <section className="system-settings-group software-update-settings" aria-labelledby="software-update-title">
      <div className="section-heading">
        <div><RefreshCw size={18} /><h3 id="software-update-title">{t("updates.title")}</h3></div>
      </div>
      <div className="software-update-summary">
        <div>
          <strong>{t("updates.currentVersion", { version: updater.buildInfo?.version ?? "—" })}</strong>
          <small>{t(`updates.variant.${updater.buildInfo?.variant ?? "standard"}`)}</small>
          <p className={updater.phase === "error" ? "error" : ""}>{t(statusKey, {
            version: updater.update?.version,
            context: platformContext(),
          })}</p>
        </div>
        <button
          className="secondary-button"
          type="button"
          disabled={busy || !updater.buildInfo?.updaterAvailable}
          onClick={() => void updater.check(true)}
        >
          <RefreshCw className={updater.phase === "checking" ? "spin" : ""} size={15} />
          {t("updates.checkNow")}
        </button>
      </div>
      <PreferenceToggle
        title={t("updates.automaticChecks")}
        checked={updater.automaticChecks}
        disabled={!updater.preferenceReady || updater.preferenceSaving || updaterUnavailable}
        onChange={(enabled) => void updater.setAutomaticChecks(enabled)}
      />
    </section>
  );
}
