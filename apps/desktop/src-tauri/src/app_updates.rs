#[cfg(windows)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(windows)]
use std::sync::Mutex;
#[cfg(windows)]
use std::time::Duration;

use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{AppHandle, State};
#[cfg(windows)]
use tauri_plugin_updater::{Update, UpdaterExt};

/// Manifest published with every release; only Windows builds ever look it up.
#[cfg(windows)]
const UPDATE_ENDPOINT: &str =
    "https://github.com/Dreaminko/VRCS/releases/latest/download/latest.json";
#[cfg(windows)]
const UPDATE_TIMEOUT: Duration = Duration::from_secs(30);

/// Public key of the release signing key used by `scripts/build-release.ps1`.
///
/// Only Windows releases carry signed updater artifacts, so the key is read for
/// Windows builds alone. Reading it on every platform let a Linux build made from a
/// shell that exported `TAURI_UPDATER_PUBLIC_KEY` look for a `windows-x86_64-*`
/// installer in the release feed.
#[cfg(windows)]
const UPDATER_PUBLIC_KEY: Option<&str> = option_env!("TAURI_UPDATER_PUBLIC_KEY");
#[cfg(not(windows))]
const UPDATER_PUBLIC_KEY: Option<&str> = None;

/// Update offered by the last check, kept until the user installs it.
#[cfg(windows)]
pub(crate) struct UpdateState {
    pending: Mutex<Option<Update>>,
    busy: AtomicBool,
}

/// Off Windows the updater is compiled out, so there is no state to keep. The type
/// stays managed by `lib.rs` so both commands keep an identical signature everywhere.
#[cfg(not(windows))]
pub(crate) struct UpdateState;

#[cfg(windows)]
impl UpdateState {
    pub(crate) fn new() -> Self {
        Self {
            pending: Mutex::new(None),
            busy: AtomicBool::new(false),
        }
    }
}

#[cfg(not(windows))]
impl UpdateState {
    pub(crate) fn new() -> Self {
        Self
    }
}

#[cfg(windows)]
struct BusyGuard<'a>(&'a AtomicBool);

#[cfg(windows)]
impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BuildInfo {
    version: &'static str,
    variant: &'static str,
    updater_available: bool,
    /// Whether the desktop shell actually created a tray icon. The settings UI
    /// must not offer "minimize to tray" when there is no tray to restore the
    /// window from: on Linux the icon is missing whenever the desktop has no
    /// StatusNotifier host.
    tray_available: bool,
    /// Platform of the running desktop shell, for UI that has to present
    /// platform-specific behavior (resize handles for the undecorated window).
    platform: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateMetadata {
    version: String,
    current_version: String,
    notes: Option<String>,
}

/// Progress events streamed to the frontend while an update installs.
///
/// Only the Windows implementation emits them, but the variants stay declared on every
/// platform: the frontend contract and the serialized shapes are platform-independent.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Clone, Serialize)]
#[serde(
    tag = "event",
    content = "data",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(crate) enum DownloadEvent {
    Started { content_length: Option<u64> },
    Progress { chunk_length: usize },
    Finished,
}

/// Reasons the update commands can be rejected; serialized as `update.*` codes.
///
/// Off Windows `Unavailable` is the only one a command can return, because there is no
/// updater artifact to check or install.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug)]
pub(crate) enum UpdateError {
    Unavailable,
    Busy,
    NoPendingUpdate,
    Failed,
}

impl Serialize for UpdateError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let code = match self {
            Self::Unavailable => "update.unavailable",
            Self::Busy => "update.busy",
            Self::NoPendingUpdate => "update.no_pending",
            Self::Failed => "update.failed",
        };
        serializer.serialize_str(code)
    }
}

fn variant() -> &'static str {
    if cfg!(feature = "cuda") {
        "cuda"
    } else {
        "standard"
    }
}

/// Target of the artifact this build installs. The release workflow signs updater
/// artifacts for the Windows target only.
#[cfg(windows)]
fn target() -> String {
    format!("windows-x86_64-{}", variant())
}

#[cfg(windows)]
fn acquire_busy(state: &UpdateState) -> Result<BusyGuard<'_>, UpdateError> {
    state
        .busy
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| UpdateError::Busy)?;
    Ok(BusyGuard(&state.busy))
}

/// Registers the updater plugin for Windows builds, which are the only ones with
/// signed updater artifacts in the release feed. Everywhere else the plugin is never
/// registered, so no update check can ever be started on those platforms.
#[cfg(windows)]
pub(crate) fn register_plugin(app: &tauri::App) -> tauri::Result<()> {
    let Some(public_key) = UPDATER_PUBLIC_KEY.filter(|key| !key.trim().is_empty()) else {
        tracing::info!("application updater is disabled because no public key was configured");
        return Ok(());
    };
    app.handle().plugin(
        tauri_plugin_updater::Builder::new()
            .pubkey(public_key)
            .target(target())
            .build(),
    )
}

#[cfg(not(windows))]
pub(crate) fn register_plugin(_app: &tauri::App) -> tauri::Result<()> {
    tracing::debug!("application updater is not registered because this platform has no artifact");
    Ok(())
}

#[tauri::command]
pub(crate) fn app_build_info(native_ui: State<'_, crate::NativeUiState>) -> BuildInfo {
    build_info(
        UPDATER_PUBLIC_KEY,
        native_ui
            .tray_available
            .load(std::sync::atomic::Ordering::Acquire),
    )
}

/// Builds the payload reported to the frontend. Kept free of Tauri state so the
/// platform contract stays unit-testable.
fn build_info(public_key: Option<&str>, tray_available: bool) -> BuildInfo {
    BuildInfo {
        version: env!("CARGO_PKG_VERSION"),
        variant: variant(),
        updater_available: public_key.is_some_and(|key| !key.trim().is_empty()),
        tray_available,
        platform: std::env::consts::OS,
    }
}

#[cfg(windows)]
#[tauri::command]
pub(crate) async fn check_for_update(
    app: AppHandle,
    state: State<'_, UpdateState>,
) -> Result<Option<UpdateMetadata>, UpdateError> {
    let public_key = UPDATER_PUBLIC_KEY
        .filter(|key| !key.trim().is_empty())
        .ok_or(UpdateError::Unavailable)?;
    let _busy = acquire_busy(&state)?;
    let endpoint = UPDATE_ENDPOINT.parse().map_err(|error| {
        tracing::error!(%error, "invalid updater endpoint");
        UpdateError::Failed
    })?;
    let before_exit_app = app.clone();
    let update = app
        .updater_builder()
        .pubkey(public_key)
        .target(target())
        .endpoints(vec![endpoint])
        .map_err(|error| {
            tracing::warn!(%error, "failed to configure application updater");
            UpdateError::Failed
        })?
        .timeout(UPDATE_TIMEOUT)
        .on_before_exit(move || crate::prepare_for_exit(&before_exit_app))
        .build()
        .map_err(|error| {
            tracing::warn!(%error, "failed to initialize application updater");
            UpdateError::Failed
        })?
        .check()
        .await
        .map_err(|error| {
            tracing::warn!(%error, "application update check failed");
            UpdateError::Failed
        })?;

    let metadata = update.as_ref().map(|update| UpdateMetadata {
        version: update.version.clone(),
        current_version: update.current_version.clone(),
        notes: update.body.clone(),
    });
    *state.pending.lock().map_err(|_| UpdateError::Failed)? = update;
    Ok(metadata)
}

/// Windows is the only platform with a signed updater artifact, so this build has no
/// target to check; the command answers `Unavailable` instead of reaching the feed.
#[cfg(not(windows))]
#[tauri::command]
#[allow(unused_variables)]
pub(crate) async fn check_for_update(
    app: AppHandle,
    state: State<'_, UpdateState>,
) -> Result<Option<UpdateMetadata>, UpdateError> {
    // `app` and `state` are unused here; they keep the IPC signature identical to the
    // Windows build so the frontend can invoke this command on every platform.
    Err(UpdateError::Unavailable)
}

#[cfg(windows)]
#[tauri::command]
pub(crate) async fn download_and_install_update(
    state: State<'_, UpdateState>,
    on_event: Channel<DownloadEvent>,
) -> Result<(), UpdateError> {
    let _busy = acquire_busy(&state)?;
    let update = state
        .pending
        .lock()
        .map_err(|_| UpdateError::Failed)?
        .take()
        .ok_or(UpdateError::NoPendingUpdate)?;
    let started = AtomicBool::new(false);

    update
        .download_and_install(
            |chunk_length, content_length| {
                if !started.swap(true, Ordering::AcqRel) {
                    let _ = on_event.send(DownloadEvent::Started { content_length });
                }
                let _ = on_event.send(DownloadEvent::Progress { chunk_length });
            },
            || {
                let _ = on_event.send(DownloadEvent::Finished);
            },
        )
        .await
        .map_err(|error| {
            tracing::error!(%error, "application update installation failed");
            UpdateError::Failed
        })
}

/// Nothing is ever offered for download on this platform, so there is no pending
/// update to install and the frontend gets the same answer as a failed check.
#[cfg(not(windows))]
#[tauri::command]
#[allow(unused_variables)]
pub(crate) async fn download_and_install_update(
    state: State<'_, UpdateState>,
    on_event: Channel<DownloadEvent>,
) -> Result<(), UpdateError> {
    // `state` and `on_event` are unused here; they keep the IPC signature identical to
    // the Windows build so the frontend can invoke this command on every platform.
    Err(UpdateError::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::{build_info, DownloadEvent};
    use serde_json::json;

    #[cfg(windows)]
    use super::{target, variant};
    #[cfg(not(windows))]
    use super::{UpdateError, UPDATER_PUBLIC_KEY};

    #[test]
    fn download_events_match_the_frontend_contract() {
        assert_eq!(
            serde_json::to_value(DownloadEvent::Started {
                content_length: Some(512),
            })
            .unwrap(),
            json!({ "event": "started", "data": { "contentLength": 512 } })
        );
        assert_eq!(
            serde_json::to_value(DownloadEvent::Progress { chunk_length: 128 }).unwrap(),
            json!({ "event": "progress", "data": { "chunkLength": 128 } })
        );
        assert_eq!(
            serde_json::to_value(DownloadEvent::Finished).unwrap(),
            json!({ "event": "finished" })
        );
    }

    #[cfg(windows)]
    #[test]
    fn updater_target_matches_build_variant() {
        let expected_variant = if cfg!(feature = "cuda") {
            "cuda"
        } else {
            "standard"
        };
        assert_eq!(variant(), expected_variant);
        assert_eq!(target(), format!("windows-x86_64-{expected_variant}"));
    }

    /// Regression guard for a non-Windows build made from a shell that exports
    /// `TAURI_UPDATER_PUBLIC_KEY`: the updater must stay unavailable instead of
    /// checking the release feed for a `windows-x86_64-*` installer.
    #[cfg(not(windows))]
    #[test]
    fn updater_is_unavailable_off_windows() {
        // The compile-time gate is what keeps the updater out of a non-Windows
        // build: the key the command reads is always `None` here, even when the
        // shell that built the crate exported `TAURI_UPDATER_PUBLIC_KEY`.
        assert!(UPDATER_PUBLIC_KEY.is_none());
        assert!(!build_info(UPDATER_PUBLIC_KEY, true).updater_available);
        // The helper itself only reports the key it is handed.
        assert!(build_info(Some("exported-public-key"), true).updater_available);
        // The code both update commands answer with here; the frontend renders it with
        // the `updates.errors.unavailable` message.
        assert_eq!(
            serde_json::to_value(UpdateError::Unavailable).unwrap(),
            json!("update.unavailable")
        );
    }

    /// The frontend gates the "minimize to tray" preference and the window resize
    /// handles on these fields, so their serialized names are a contract.
    #[test]
    fn build_info_reports_the_runtime_capabilities() {
        let info = serde_json::to_value(build_info(None, true)).unwrap();
        assert_eq!(info["trayAvailable"], json!(true));
        assert_eq!(info["updaterAvailable"], json!(false));
        assert_eq!(info["platform"], json!(std::env::consts::OS));
        assert_eq!(
            serde_json::to_value(build_info(None, false)).unwrap()["trayAvailable"],
            json!(false)
        );
    }
}
