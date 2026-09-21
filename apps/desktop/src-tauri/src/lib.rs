mod app_updates;
mod diagnostics;
mod vr_overlay;

use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Manager, RunEvent, State, WindowEvent};
use tauri_plugin_store::StoreExt;
use uuid::Uuid;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CoreConnection {
    http_url: String,
    ws_url: String,
    token: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CoreStartup {
    state: CoreStartupState,
    error: Option<String>,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
enum CoreStartupState {
    Starting,
    Ready,
    Failed,
}

#[derive(Clone)]
struct CoreLaunchOptions {
    config_path: PathBuf,
    port: u16,
    token: String,
}

struct CoreRuntime {
    handle: Mutex<Option<vrcs_core::CoreHandle>>,
    launch_task: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    startup: Mutex<CoreStartup>,
    options: CoreLaunchOptions,
    stop_requested: AtomicBool,
}

pub(crate) struct NativeUiState {
    show_item: Mutex<Option<MenuItem<tauri::Wry>>>,
    quit_item: Mutex<Option<MenuItem<tauri::Wry>>>,
    compact_topmost: AtomicBool,
    pub(crate) tray_available: AtomicBool,
}

const DEFAULT_CORE_PORT: u16 = 8766;
const VRCX_REPOSITORY_URL: &str = "https://github.com/Map1en/VRCX-0";

fn available_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .and_then(|listener| listener.local_addr())
        .map(|address| address.port())
        .expect("failed to allocate a local port for VRCS Core")
}

fn core_connection_config() -> CoreConnection {
    let token = Uuid::new_v4().simple().to_string();
    if cfg!(debug_assertions) {
        return CoreConnection {
            http_url: format!("http://127.0.0.1:{DEFAULT_CORE_PORT}"),
            ws_url: format!("ws://127.0.0.1:{DEFAULT_CORE_PORT}/ws"),
            token,
        };
    }

    let port = available_port();
    CoreConnection {
        http_url: format!("http://127.0.0.1:{port}"),
        ws_url: format!("ws://127.0.0.1:{port}/ws"),
        token,
    }
}

#[tauri::command]
fn core_connection(connection: State<'_, CoreConnection>) -> CoreConnection {
    connection.inner().clone()
}

#[tauri::command]
fn core_startup(runtime: State<'_, CoreRuntime>) -> Result<CoreStartup, String> {
    runtime
        .startup
        .lock()
        .map(|startup| startup.clone())
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn retry_core(app: tauri::AppHandle) -> Result<(), String> {
    launch_core(&app)
}

#[tauri::command]
fn write_glossary_file(path: PathBuf, contents: String) -> Result<(), String> {
    std::fs::write(path, contents.as_bytes()).map_err(|error| error.to_string())
}

#[tauri::command]
fn open_vrcx_repository() -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let mut command = std::process::Command::new("explorer.exe");
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = std::process::Command::new("xdg-open");

    command
        .arg(VRCX_REPOSITORY_URL)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Failed to open VRCX-0 repository: {error}"))
}

fn launch_core(app: &tauri::AppHandle) -> Result<(), String> {
    let runtime = app.state::<CoreRuntime>();
    {
        let mut startup = runtime.startup.lock().map_err(|error| error.to_string())?;
        if matches!(
            startup.state,
            CoreStartupState::Starting | CoreStartupState::Ready
        ) {
            return Ok(());
        }
        *startup = CoreStartup {
            state: CoreStartupState::Starting,
            error: None,
        };
    }
    runtime.stop_requested.store(false, Ordering::Release);

    let options = runtime.options.clone();
    let app = app.clone();
    let launch_task = tauri::async_runtime::spawn(async move {
        let started = Instant::now();
        let result = vrcs_core::start_with_deferred_vad(vrcs_core::CoreOptions {
            config_path: options.config_path,
            host: Some("127.0.0.1".into()),
            port: Some(options.port),
            session_token: Some(options.token),
            vad_model_path: None,
            asr_model_dir: None,
        })
        .await;
        let runtime = app.state::<CoreRuntime>();
        match result {
            Ok(core) if runtime.stop_requested.load(Ordering::Acquire) => {
                if let Err(error) = core.shutdown().await {
                    tracing::warn!(%error, "Core shutdown after cancelled startup failed");
                }
            }
            Ok(core) => {
                let presentation_events = core.subscribe_presentation_events();
                let vr_overlay_config = core.subscribe_vr_overlay_config();
                if let Err(error) = app
                    .state::<vr_overlay::Manager>()
                    .start(presentation_events, vr_overlay_config)
                {
                    tracing::warn!(%error, "VR Overlay startup failed");
                }
                *runtime.handle.lock().expect("core runtime lock poisoned") = Some(core);
                *runtime.startup.lock().expect("core startup lock poisoned") = CoreStartup {
                    state: CoreStartupState::Ready,
                    error: None,
                };
                tracing::info!(
                    elapsed_ms = started.elapsed().as_millis(),
                    "desktop Core startup ready"
                );
            }
            Err(error) => {
                let report_id = diagnostics::record_error(
                    app.state::<diagnostics::DiagnosticState>().inner(),
                    "core",
                    "core_startup",
                    "core.startup_failed",
                    &error,
                    None,
                );
                tracing::info!(
                    %report_id,
                    elapsed_ms = started.elapsed().as_millis(),
                    "desktop Core startup failure recorded"
                );
                *runtime.startup.lock().expect("core startup lock poisoned") = CoreStartup {
                    state: CoreStartupState::Failed,
                    error: Some(error),
                };
            }
        }
    });
    *runtime
        .launch_task
        .lock()
        .map_err(|error| error.to_string())? = Some(launch_task);
    Ok(())
}

#[tauri::command]
fn update_native_labels(
    show: String,
    quit: String,
    native_ui: State<'_, NativeUiState>,
) -> Result<(), String> {
    if let Some(item) = native_ui
        .show_item
        .lock()
        .map_err(|error| error.to_string())?
        .as_ref()
    {
        item.set_text(show).map_err(|error| error.to_string())?;
    }
    if let Some(item) = native_ui
        .quit_item
        .lock()
        .map_err(|error| error.to_string())?
        .as_ref()
    {
        item.set_text(quit).map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[cfg(windows)]
fn apply_native_topmost(window: &tauri::WebviewWindow, enabled: bool) -> Result<(), String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE, HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE,
        SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, WS_EX_TOPMOST,
    };

    let hwnd = window.hwnd().map_err(|error| error.to_string())?.0 as _;
    let insert_after = if enabled {
        HWND_TOPMOST
    } else {
        HWND_NOTOPMOST
    };
    let result = unsafe {
        SetWindowPos(
            hwnd,
            insert_after,
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOOWNERZORDER | SWP_NOSIZE,
        )
    };
    if result == 0 {
        return Err(format!(
            "Windows failed to update the compact subtitle window Z-order: {}",
            std::io::Error::last_os_error()
        ));
    }

    let extended_style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
    let actual = extended_style & WS_EX_TOPMOST != 0;
    if actual != enabled {
        return Err("Windows did not retain the requested compact subtitle Z-order".into());
    }
    Ok(())
}

fn apply_compact_topmost(
    window: &tauri::WebviewWindow,
    enabled: bool,
    reason: &'static str,
) -> Result<(), String> {
    window
        .set_always_on_top(enabled)
        .map_err(|error| error.to_string())?;

    #[cfg(windows)]
    apply_native_topmost(window, enabled)?;

    #[cfg(not(windows))]
    if window
        .is_always_on_top()
        .map_err(|error| error.to_string())?
        != enabled
    {
        return Err("The compact subtitle window did not retain its requested Z-order".into());
    }

    tracing::debug!(enabled, reason, "compact window topmost state applied");
    Ok(())
}

#[tauri::command]
fn set_compact_window_topmost(
    window: tauri::WebviewWindow,
    enabled: bool,
    native_ui: State<'_, NativeUiState>,
) -> Result<(), String> {
    apply_compact_topmost(&window, enabled, "mode_change")?;
    native_ui.compact_topmost.store(enabled, Ordering::Release);
    Ok(())
}

fn reassert_compact_topmost(window: &tauri::WebviewWindow, reason: &'static str) {
    if !window
        .app_handle()
        .state::<NativeUiState>()
        .compact_topmost
        .load(Ordering::Acquire)
    {
        return;
    }
    if let Err(error) = apply_compact_topmost(window, true, reason) {
        tracing::warn!(%error, reason, "compact window topmost reassertion failed");
    }
}

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        reassert_compact_topmost(&window, "window_shown");
        let _ = window.set_focus();
    }
}

/// Decides whether a close request on the main window hides it into the tray.
///
/// Hiding is only safe while the tray icon exists, because the tray (or its
/// "Show VRCS" item) is the only way back to a hidden window. On Linux the tray
/// is missing whenever the desktop has no StatusNotifier/appindicator host, so
/// hiding there would leave a running process whose window cannot be restored;
/// the close must really close instead.
fn should_hide_on_close(preference: bool, tray_available: bool) -> bool {
    preference && tray_available
}

/// Whether a tray icon can actually be seen on this desktop.
///
/// On Linux `TrayIconBuilder::build` only proves that the appindicator library loaded: it never
/// asks the session bus, so it succeeds on a desktop that has no StatusNotifier host, where the
/// icon stays invisible. The close-to-tray path must not trust it, or the window would hide
/// behind an icon that is not on screen. A bus that cannot be reached, or an unexpected reply,
/// counts as available: a probe failure must not silently remove a tray that works.
#[cfg(target_os = "linux")]
fn tray_host_available() -> bool {
    const WATCHERS: [&str; 2] = [
        "org.kde.StatusNotifierWatcher",
        "org.x.StatusNotifierWatcher",
    ];

    let Ok(connection) = zbus::blocking::Connection::session() else {
        return true;
    };
    WATCHERS.iter().any(|watcher| {
        connection
            .call_method(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                Some("org.freedesktop.DBus"),
                "NameHasOwner",
                &(*watcher,),
            )
            .ok()
            .and_then(|reply| reply.body().deserialize::<bool>().ok())
            .unwrap_or(true)
    })
}

/// Windows and macOS always have a tray area; nothing to probe.
#[cfg(not(target_os = "linux"))]
fn tray_host_available() -> bool {
    true
}

fn minimize_to_tray_enabled(app: &tauri::AppHandle) -> bool {
    app.store("preferences.json")
        .ok()
        .and_then(|store| store.get("minimizeToTray"))
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(crate) fn prepare_for_exit(app: &tauri::AppHandle) {
    app.state::<vr_overlay::Manager>().stop();
    stop_core(app);
}

fn stop_core(app: &tauri::AppHandle) {
    let runtime = app.state::<CoreRuntime>();
    runtime.stop_requested.store(true, Ordering::Release);
    let launch_task = runtime
        .launch_task
        .lock()
        .expect("core launch task lock poisoned")
        .take();
    let app_handle = app.clone();
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
    tauri::async_runtime::spawn(async move {
        if let Some(task) = launch_task {
            let _ = task.await;
        }
        let core = app_handle
            .state::<CoreRuntime>()
            .handle
            .lock()
            .expect("core runtime lock poisoned")
            .take();
        let result = match core {
            Some(core) => core.shutdown().await,
            None => Ok(()),
        };
        let _ = done_tx.send(result);
    });

    match done_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            diagnostics::record_error(
                app.state::<diagnostics::DiagnosticState>().inner(),
                "core",
                "core_shutdown",
                "core.shutdown_failed",
                &error,
                None,
            );
        }
        Err(error) => {
            diagnostics::record_error(
                app.state::<diagnostics::DiagnosticState>().inner(),
                "core",
                "core_shutdown",
                "core.shutdown_timeout",
                &error.to_string(),
                None,
            );
        }
    }
}

/// Resolves the directory that holds the VRCS configuration, history and models.
///
/// Windows and macOS keep the historical `.vrcs` directory inside the app's
/// local data directory. Linux uses `$XDG_DATA_HOME/vrcs` instead: a dot-prefixed
/// directory is not the XDG convention for an application directory, and `vrcs`
/// is exactly the root the Core uses for `credentials.json`, so the old name
/// split one installation across a visible and a hidden directory.
fn resolve_data_dir(local_data_dir: PathBuf) -> PathBuf {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let current = local_data_dir.join("vrcs");
        let legacy = local_data_dir.join(".vrcs");
        migrate_legacy_data_dir(&current, &legacy)
    }

    #[cfg(not(all(unix, not(target_os = "macos"))))]
    {
        local_data_dir.join(".vrcs")
    }
}

/// Moves a pre-existing `.vrcs` directory to the new Linux data root and returns
/// the directory the app must use.
///
/// The rename only runs while the legacy directory is the surviving one, so an
/// already migrated install is never touched. When the rename fails the legacy
/// directory is returned rather than `current`: starting from an empty data root
/// while the user's configuration and history are still on disk is the one
/// outcome that must not happen.
#[cfg(all(unix, not(target_os = "macos")))]
fn migrate_legacy_data_dir(current: &std::path::Path, legacy: &std::path::Path) -> PathBuf {
    if current.exists() || !legacy.exists() {
        return current.to_path_buf();
    }

    match std::fs::rename(legacy, current) {
        Ok(()) => current.to_path_buf(),
        Err(error) => {
            tracing::warn!(
                %error,
                legacy = %legacy.display(),
                current = %current.display(),
                "VRCS data directory could not be migrated; continuing with the legacy path"
            );
            legacy.to_path_buf()
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let log_dir = diagnostics::desktop_log_dir();
    let _logging_guard = match vrcs_core::init_tracing(Some(&log_dir)) {
        Ok(guard) => Some(guard),
        Err(error) => {
            eprintln!("VRCS file logging is unavailable: {error}");
            vrcs_core::init_tracing(None).ok()
        }
    };
    let diagnostic_state = diagnostics::DiagnosticState::new(log_dir.clone());
    diagnostics::install_panic_hook(diagnostic_state.clone());
    tracing::info!(
        session_id = %diagnostic_state.session_id(),
        version = env!("CARGO_PKG_VERSION"),
        "VRCS desktop starting"
    );

    let connection = core_connection_config();
    let setup_connection = connection.clone();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            show_main_window(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .manage(connection)
        .manage(diagnostic_state)
        .manage(app_updates::UpdateState::new())
        .manage(NativeUiState {
            show_item: Mutex::new(None),
            quit_item: Mutex::new(None),
            compact_topmost: AtomicBool::new(false),
            tray_available: AtomicBool::new(false),
        })
        .invoke_handler(tauri::generate_handler![
            core_connection,
            core_startup,
            retry_core,
            write_glossary_file,
            open_vrcx_repository,
            diagnostics::diagnostic_status,
            diagnostics::report_frontend_error,
            diagnostics::open_log_directory,
            diagnostics::export_error_report,
            app_updates::app_build_info,
            app_updates::check_for_update,
            app_updates::download_and_install_update,
            update_native_labels,
            set_compact_window_topmost,
            vr_overlay::vr_overlay_status,
            vr_overlay::vr_overlay_retry,
            vr_overlay::vr_overlay_show_sample,
            vr_overlay::vr_overlay_hide_sample
        ])
        .setup(move |app| {
            app_updates::register_plugin(app)?;
            app.store("preferences.json")?;

            let show_item = MenuItem::with_id(app, "show", "Show VRCS", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit VRCS", true, None::<&str>)?;
            let tray_menu = Menu::with_items(app, &[&show_item, &quit_item])?;
            let mut tray = TrayIconBuilder::new()
                .tooltip("VRCS")
                .menu(&tray_menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "show" => show_main_window(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main_window(tray.app_handle());
                    }
                });
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }

            // On Linux the tray needs a StatusNotifier host: an appindicator
            // library plus a panel that implements the protocol, which many
            // minimal desktops lack. Losing the icon must never stop VRCS from
            // starting, so a tray failure is logged and startup continues.
            let native_ui = app.state::<NativeUiState>();
            match tray.build(app) {
                Ok(_) => {
                    // 图标建好了不等于看得见：没有 host 时它不会出现在面板上，
                    // 因此"最小化到托盘"的开关要以探测结果为准。
                    let visible = tray_host_available();
                    if !visible {
                        tracing::warn!(
                            "no StatusNotifier host on the session bus; the tray icon stays \
                             invisible and minimize-to-tray is disabled"
                        );
                    }
                    native_ui.tray_available.store(visible, Ordering::Release);
                    *native_ui
                        .show_item
                        .lock()
                        .expect("native UI state lock poisoned") = Some(show_item.clone());
                    *native_ui
                        .quit_item
                        .lock()
                        .expect("native UI state lock poisoned") = Some(quit_item.clone());
                }
                Err(error) => {
                    tracing::warn!(
                        %error,
                        "system tray is unavailable; VRCS keeps running without a tray icon"
                    );
                }
            }

            let data_dir = resolve_data_dir(app.path().local_data_dir()?);
            std::fs::create_dir_all(data_dir.join("models"))?;

            let port = setup_connection
                .http_url
                .rsplit(':')
                .next()
                .and_then(|value| value.parse().ok())
                .ok_or_else(|| std::io::Error::other("core URL is missing a valid port"))?;
            app.manage(vr_overlay::Manager::new(app.handle().clone()));
            app.manage(CoreRuntime {
                handle: Mutex::new(None),
                launch_task: Mutex::new(None),
                startup: Mutex::new(CoreStartup {
                    state: CoreStartupState::Failed,
                    error: None,
                }),
                options: CoreLaunchOptions {
                    config_path: data_dir.join("config.json"),
                    port,
                    token: setup_connection.token.clone(),
                },
                stop_requested: AtomicBool::new(false),
            });
            launch_core(app.handle()).map_err(std::io::Error::other)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == "main" {
                if matches!(event, WindowEvent::Focused(false)) {
                    if let Some(window) = window.app_handle().get_webview_window("main") {
                        reassert_compact_topmost(&window, "focus_lost");
                    }
                }
                if let WindowEvent::CloseRequested { api, .. } = event {
                    let tray_available = window
                        .app_handle()
                        .state::<NativeUiState>()
                        .tray_available
                        .load(Ordering::Acquire);
                    let hide = should_hide_on_close(
                        minimize_to_tray_enabled(window.app_handle()),
                        tray_available,
                    );
                    if hide {
                        api.prevent_close();
                        let _ = window.hide();
                    }
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building VRCS");

    app.run(|app_handle, event| {
        if matches!(event, RunEvent::Exit | RunEvent::ExitRequested { .. }) {
            prepare_for_exit(app_handle);
        }
    });
}

pub fn release_self_test() -> Result<(), String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "vrcs-release-self-test-{}-{nonce}",
        std::process::id()
    ));
    let result = tauri::async_runtime::block_on(async {
        let handle = vrcs_core::start(vrcs_core::CoreOptions {
            config_path: directory.join("config.json"),
            host: Some("127.0.0.1".into()),
            port: Some(0),
            session_token: None,
            vad_model_path: None,
            asr_model_dir: None,
        })
        .await?;
        let vad_error = (handle.vad_backend() != "silero-onnx"
            || handle.vad_model_version() != Some("v6.2.1"))
        .then(|| "Silero v6.2.1 failed the first-start download and load self-test".to_string());
        let shutdown_result = handle.shutdown().await;
        if let Some(error) = vad_error {
            return Err(error);
        }
        shutdown_result
    });
    let _ = std::fs::remove_dir_all(&directory);
    result
}

#[cfg(test)]
mod tests {
    use super::{core_connection_config, should_hide_on_close, DEFAULT_CORE_PORT};

    const CAPABILITIES: &str = include_str!("../capabilities/default.json");

    #[test]
    fn custom_window_actions_are_allowed() {
        for permission in [
            "core:window:allow-start-dragging",
            "core:window:allow-minimize",
            "core:window:allow-toggle-maximize",
            "core:window:allow-close",
            "core:window:allow-set-resizable",
            "autostart:allow-enable",
            "autostart:allow-disable",
            "autostart:allow-is-enabled",
            "dialog:allow-open",
            "dialog:allow-save",
            "store:default",
        ] {
            assert!(CAPABILITIES.contains(permission), "missing {permission}");
        }
    }

    #[test]
    fn development_core_port_avoids_anki_and_vrchat_osc_defaults() {
        assert_eq!(DEFAULT_CORE_PORT, 8766);
        assert!(![8765, 9000, 9001].contains(&DEFAULT_CORE_PORT));
        assert!(!core_connection_config().token.is_empty());
    }

    #[test]
    fn closing_only_hides_the_window_with_a_tray_and_the_preference() {
        assert!(should_hide_on_close(true, true));
        assert!(!should_hide_on_close(true, false));
        assert!(!should_hide_on_close(false, true));
        assert!(!should_hide_on_close(false, false));
    }

    /// 探测必须如实反映总线：没有 watcher 就是"不可用"，而不是"库加载成功"。
    #[test]
    #[cfg(target_os = "linux")]
    fn tray_host_probe_reports_no_watcher_on_a_private_bus() {
        // 子进程跑在 `dbus-run-session` 新建的会话总线上，上面没有任何 StatusNotifier host。
        if std::env::var_os("VRCS_TRAY_PROBE_CHILD").is_some() {
            assert!(!super::tray_host_available());
            return;
        }
        let status = std::process::Command::new("dbus-run-session")
            .arg("--")
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::tray_host_probe_reports_no_watcher_on_a_private_bus",
                "--nocapture",
            ])
            .env("VRCS_TRAY_PROBE_CHILD", "1")
            .status();
        match status {
            Ok(status) => assert!(status.success(), "the private-bus probe failed: {status}"),
            // 没装 dbus-run-session 时不假装测过。
            Err(error) => eprintln!("skipping the private-bus probe test: {error}"),
        }
    }

    /// 总线不可达时按"有托盘"处理：探测失败不该把能用的托盘关掉。
    #[test]
    #[cfg(target_os = "linux")]
    fn tray_host_probe_treats_an_unreachable_bus_as_available() {
        if std::env::var_os("VRCS_TRAY_PROBE_CHILD").is_some() {
            assert!(super::tray_host_available());
            return;
        }
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::tray_host_probe_treats_an_unreachable_bus_as_available",
                "--nocapture",
            ])
            .env("VRCS_TRAY_PROBE_CHILD", "1")
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                "unix:path=/nonexistent-vrcs-probe-bus",
            )
            .status()
            .expect("re-exec the test binary");
        assert!(
            status.success(),
            "the unreachable-bus probe failed: {status}"
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    fn temp_data_root(label: &str) -> std::path::PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "vrcs-data-dir-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn legacy_hidden_data_directory_is_migrated_into_the_xdg_root() {
        let root = temp_data_root("migrate");
        let legacy = root.join(".vrcs");
        std::fs::create_dir_all(legacy.join("models")).unwrap();
        std::fs::write(legacy.join("config.json"), "legacy").unwrap();

        let resolved = super::resolve_data_dir(root.clone());

        assert_eq!(resolved, root.join("vrcs"));
        assert!(!legacy.exists());
        assert_eq!(
            std::fs::read_to_string(resolved.join("config.json")).unwrap(),
            "legacy"
        );
        assert!(resolved.join("models").is_dir());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn existing_xdg_data_directory_wins_over_the_legacy_one() {
        let root = temp_data_root("both");
        let legacy = root.join(".vrcs");
        let current = root.join("vrcs");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::create_dir_all(&current).unwrap();
        std::fs::write(legacy.join("config.json"), "legacy").unwrap();
        std::fs::write(current.join("config.json"), "current").unwrap();

        let resolved = super::resolve_data_dir(root.clone());

        assert_eq!(resolved, current);
        assert_eq!(
            std::fs::read_to_string(legacy.join("config.json")).unwrap(),
            "legacy"
        );
        assert_eq!(
            std::fs::read_to_string(resolved.join("config.json")).unwrap(),
            "current"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn fresh_install_uses_the_xdg_data_directory() {
        let root = temp_data_root("fresh");

        let resolved = super::resolve_data_dir(root.clone());

        assert_eq!(resolved, root.join("vrcs"));
        assert!(!resolved.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn failed_migration_keeps_using_the_legacy_data_directory() {
        let root = temp_data_root("fallback");
        let legacy = root.join(".vrcs");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("config.json"), "legacy").unwrap();
        // A target whose parent directory does not exist cannot be renamed onto.
        let unreachable = root.join("missing").join("vrcs");

        let resolved = super::migrate_legacy_data_dir(&unreachable, &legacy);

        assert_eq!(resolved, legacy);
        assert_eq!(
            std::fs::read_to_string(resolved.join("config.json")).unwrap(),
            "legacy"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
