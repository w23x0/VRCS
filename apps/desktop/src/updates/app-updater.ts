import { Channel, invoke, isTauri } from "@tauri-apps/api/core";

export interface AppBuildInfo {
  version: string;
  variant: "standard" | "cuda";
  updaterAvailable: boolean;
  /** False when the desktop shell could not create a tray icon (no tray to restore a hidden window from). */
  trayAvailable: boolean;
  /** Platform of the desktop shell: "linux", "windows", "macos"; "browser" outside the shell. */
  platform: string;
}

export interface AppUpdateMetadata {
  version: string;
  currentVersion: string;
  notes: string | null;
}

export type UpdateDownloadEvent =
  | { event: "started"; data: { contentLength: number | null } }
  | { event: "progress"; data: { chunkLength: number } }
  | { event: "finished" };

export async function loadAppBuildInfo(): Promise<AppBuildInfo> {
  if (!isTauri()) {
    // Outside the desktop shell there is no window to hide into a tray, so the
    // tray-dependent preference stays enabled rather than being disabled by a
    // capability answer that does not apply to this context.
    return {
      version: "development",
      variant: "standard",
      updaterAvailable: false,
      trayAvailable: true,
      platform: "browser",
    };
  }
  return invoke<AppBuildInfo>("app_build_info");
}

export async function checkForAppUpdate(): Promise<AppUpdateMetadata | null> {
  if (!isTauri()) return null;
  return invoke<AppUpdateMetadata | null>("check_for_update");
}

export async function downloadAndInstallAppUpdate(
  onEvent: (event: UpdateDownloadEvent) => void,
): Promise<void> {
  if (!isTauri()) return;
  const channel = new Channel<UpdateDownloadEvent>();
  channel.onmessage = onEvent;
  await invoke("download_and_install_update", { onEvent: channel });
}

export function updaterErrorCode(reason: unknown): string {
  return typeof reason === "string" && reason.startsWith("update.")
    ? reason.slice("update.".length)
    : "failed";
}
