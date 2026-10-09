//! Thin Windows shell around the Delta web app.
//!
//! Production opens https://delta.galaxyco.ai/sign-in. Dev opens http://localhost:3000/sign-in.
//! The start page is set by `app.windows[0].url` ("sign-in") in tauri.conf.json,
//! resolved against `build.frontendDist` (release) or `build.devUrl` (dev).
//! Signed-in users are redirected from /sign-in to the app by Clerk.
//! Only the canonical host is allowed. Do not point this at any other deployment.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::menu::{CheckMenuItem, Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Manager, WebviewWindow};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_deep_link::DeepLinkExt;
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_updater::UpdaterExt;
use tauri_plugin_window_state::{AppHandleExt, StateFlags};
use url::Url;

const PROD_ORIGIN: &str = "https://delta.galaxyco.ai";
const DEV_ORIGIN: &str = "http://localhost:3000";
const MAIN_WINDOW: &str = "main";

fn app_origin() -> &'static str {
    if cfg!(debug_assertions) {
        DEV_ORIGIN
    } else {
        PROD_ORIGIN
    }
}

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn hide_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.hide();
    }
}

fn open_external(app: &tauri::AppHandle, target: &str) {
    if let Err(error) = app.opener().open_url(target, None::<&str>) {
        eprintln!("delta-desktop: failed to open {target}: {error}");
    }
}

/// Parse a `delta://` URL. Auth tickets navigate the webview to the documented
/// consume path (a follow-up web route; see desktop/README.md). Anything else
/// just brings the window forward.
fn handle_deep_link(app: &tauri::AppHandle, raw: &str) {
    let Ok(url) = Url::parse(raw) else {
        show_main_window(app);
        return;
    };
    if url.scheme() != "delta" {
        show_main_window(app);
        return;
    }

    let host = url.host_str().unwrap_or("");
    let path = url.path().trim_matches('/');
    let ticket = url
        .query_pairs()
        .find(|(key, _)| key == "ticket")
        .map(|(_, value)| value.into_owned());

    let is_auth = host.eq_ignore_ascii_case("auth")
        || path.eq_ignore_ascii_case("auth")
        || path.eq_ignore_ascii_case("auth/consume");

    show_main_window(app);

    if is_auth {
        if let Some(ticket) = ticket.filter(|value| !value.is_empty()) {
            if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
                navigate_to_ticket(&window, &ticket);
            }
        }
    }
}

fn navigate_to_ticket(window: &WebviewWindow, ticket: &str) {
    let mut consume = match Url::parse(&format!("{}/desktop/auth/consume", app_origin())) {
        Ok(url) => url,
        Err(error) => {
            eprintln!("delta-desktop: bad consume origin: {error}");
            return;
        }
    };
    consume
        .query_pairs_mut()
        .append_pair("ticket", ticket)
        .append_pair("source", "desktop");
    if let Err(error) = window.navigate(consume) {
        eprintln!("delta-desktop: navigate to consume failed: {error}");
    }
}

fn notify(app: &tauri::AppHandle, title: &str, body: &str) {
    if let Err(error) = app.notification().builder().title(title).body(body).show() {
        eprintln!("delta-desktop: notification failed: {error}");
    }
}

// ---------------------------------------------------------------------------
// Silent auto-update
//
// Policy (also documented in desktop/README.md):
// 1. About 5 seconds after launch, check the endpoint. If an update exists it
//    is downloaded and installed right away, because the app has only just
//    opened and nothing is lost by restarting.
// 2. While running, check again every 4 hours. A found update is downloaded
//    in the background and held until a quiet moment: the main window is
//    hidden to the tray, minimized, or has not had focus for 15 minutes.
//    That condition is re-evaluated every minute.
// 3. Install runs the NSIS installer in quiet mode (`/S /UPDATE /R`). The app
//    exits, the per-user installer replaces files with no UI and no admin
//    prompt, then relaunches Delta with the same command-line arguments.
// 4. On the first launch of a new version, a single toast says
//    "Delta updated to vX". There are no prompts or confirmation dialogs.
// Debug builds never auto-update.
// ---------------------------------------------------------------------------

const STARTUP_CHECK_DELAY: Duration = Duration::from_secs(5);
const PERIODIC_CHECK_INTERVAL: Duration = Duration::from_secs(4 * 60 * 60);
const QUIET_POLL_INTERVAL: Duration = Duration::from_secs(60);
const UNFOCUSED_IDLE: Duration = Duration::from_secs(15 * 60);
const VERSION_MARKER: &str = "last-run-version";
/// Written just before a background install when the window is out of sight,
/// so the relaunched app stays in the tray instead of popping up.
const START_HIDDEN_MARKER: &str = "start-hidden-once";

static UPDATE_BUSY: AtomicBool = AtomicBool::new(false);
static WINDOW_FOCUSED: AtomicBool = AtomicBool::new(true);
static LAST_FOCUS_CHANGE: Mutex<Option<Instant>> = Mutex::new(None);

/// Window size, position and maximized state are persisted. Visibility is not:
/// launch code decides whether to show the window or stay in the tray.
fn window_state_flags() -> StateFlags {
    StateFlags::all() - StateFlags::VISIBLE
}

fn record_focus(focused: bool) {
    WINDOW_FOCUSED.store(focused, Ordering::SeqCst);
    if let Ok(mut guard) = LAST_FOCUS_CHANGE.lock() {
        *guard = Some(Instant::now());
    }
}

/// True when restarting now is unlikely to interrupt the user.
fn is_quiet_moment(app: &tauri::AppHandle) -> bool {
    let Some(window) = app.get_webview_window(MAIN_WINDOW) else {
        return true;
    };
    if !window.is_visible().unwrap_or(true) || window.is_minimized().unwrap_or(false) {
        return true;
    }
    if WINDOW_FOCUSED.load(Ordering::SeqCst) {
        return false;
    }
    LAST_FOCUS_CHANGE
        .lock()
        .ok()
        .and_then(|guard| *guard)
        .map(|since| since.elapsed() >= UNFOCUSED_IDLE)
        .unwrap_or(false)
}

/// Installs the downloaded update. On Windows this launches the quiet NSIS
/// installer and exits the process; the installer relaunches Delta.
fn apply_update(app: &tauri::AppHandle, update: &tauri_plugin_updater::Update, bytes: &[u8]) {
    let _ = app.save_window_state(window_state_flags());
    let out_of_sight = app
        .get_webview_window(MAIN_WINDOW)
        .map(|w| !w.is_visible().unwrap_or(true) || w.is_minimized().unwrap_or(false))
        .unwrap_or(false);
    if out_of_sight {
        if let Ok(dir) = app.path().app_config_dir() {
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(dir.join(START_HIDDEN_MARKER), "1");
        }
    }
    if let Err(error) = update.install(bytes) {
        eprintln!("delta-desktop: update install failed: {error}");
        return;
    }
    // Only reached on platforms where install does not exit the process.
    app.restart();
}

/// Checks for an update and, if one exists, downloads and installs it.
/// `wait_for_quiet` holds the install until `is_quiet_moment` is true.
/// Returns Ok(false) when already up to date.
async fn update_once(app: &tauri::AppHandle, wait_for_quiet: bool) -> Result<bool, String> {
    if UPDATE_BUSY.swap(true, Ordering::SeqCst) {
        return Ok(true);
    }
    let result = async {
        let updater = app.updater().map_err(|e| e.to_string())?;
        let Some(update) = updater.check().await.map_err(|e| e.to_string())? else {
            return Ok(false);
        };
        let bytes = update
            .download(|_, _| {}, || {})
            .await
            .map_err(|e| e.to_string())?;
        if wait_for_quiet {
            while !is_quiet_moment(app) {
                tokio::time::sleep(QUIET_POLL_INTERVAL).await;
            }
        }
        apply_update(app, &update, &bytes);
        Ok(true)
    }
    .await;
    UPDATE_BUSY.store(false, Ordering::SeqCst);
    result
}

fn start_auto_updater(app: tauri::AppHandle) {
    if cfg!(debug_assertions) {
        return;
    }
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(STARTUP_CHECK_DELAY).await;
        if let Err(error) = update_once(&app, false).await {
            eprintln!("delta-desktop: startup update failed: {error}");
        }
        loop {
            tokio::time::sleep(PERIODIC_CHECK_INTERVAL).await;
            if let Err(error) = update_once(&app, true).await {
                eprintln!("delta-desktop: periodic update failed: {error}");
            }
        }
    });
}

/// Tray "Check for updates": the user asked, so install immediately.
fn manual_update(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        match update_once(&app, false).await {
            Ok(true) => {}
            Ok(false) => notify(&app, "Delta", "You are on the latest version."),
            Err(error) => notify(&app, "Delta", &format!("Update check failed: {error}")),
        }
    });
}

/// Shows "Delta updated to vX" once, on the first launch of a new version.
fn announce_new_version(app: &tauri::AppHandle) {
    let current = app.package_info().version.to_string();
    let Ok(dir) = app.path().app_config_dir() else {
        return;
    };
    let marker = dir.join(VERSION_MARKER);
    let previous = std::fs::read_to_string(&marker).ok();
    if previous.as_deref().map(str::trim) != Some(current.as_str()) {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(&marker, &current);
        // No marker means a fresh install (or an upgrade from 0.1.1 or older,
        // which never wrote one). Only announce real upgrades.
        if previous.is_some() {
            notify(app, "Delta", &format!("Delta updated to v{current}"));
        }
    }
}

// ---------------------------------------------------------------------------
// Start with Windows
//
// Enabled by default on the first run of a build that has this feature (a
// marker file records that the default was applied, so turning it off in the
// tray sticks). Login launches pass `--autostart` and start hidden in the tray.
// ---------------------------------------------------------------------------

const AUTOSTART_ARG: &str = "--autostart";
const AUTOSTART_MARKER: &str = "autostart-default-applied";

fn launched_by_autostart() -> bool {
    std::env::args().any(|arg| arg == AUTOSTART_ARG)
}

/// True (once) if the previous run asked this launch to stay in the tray.
fn take_start_hidden_marker(app: &tauri::AppHandle) -> bool {
    let Ok(dir) = app.path().app_config_dir() else {
        return false;
    };
    std::fs::remove_file(dir.join(START_HIDDEN_MARKER)).is_ok()
}

fn apply_autostart_default(app: &tauri::AppHandle) {
    if cfg!(debug_assertions) {
        return;
    }
    let Ok(dir) = app.path().app_config_dir() else {
        return;
    };
    let marker = dir.join(AUTOSTART_MARKER);
    if marker.exists() {
        return;
    }
    if let Err(error) = app.autolaunch().enable() {
        eprintln!("delta-desktop: enabling autostart failed: {error}");
        return;
    }
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(&marker, "1");
}

fn toggle_autostart(app: &tauri::AppHandle, item: &CheckMenuItem<tauri::Wry>) {
    let manager = app.autolaunch();
    let enabled = manager.is_enabled().unwrap_or(false);
    let result = if enabled { manager.disable() } else { manager.enable() };
    if let Err(error) = result {
        eprintln!("delta-desktop: toggling autostart failed: {error}");
    }
    let _ = item.set_checked(manager.is_enabled().unwrap_or(!enabled));
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            show_main_window(app);
            for argument in argv {
                if argument.starts_with("delta://") {
                    handle_deep_link(app, &argument);
                }
            }
        }))
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(window_state_flags())
                .build(),
        )
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec![AUTOSTART_ARG]),
        ))
        .setup(|app| {
            let handle = app.handle().clone();

            let show_item = MenuItem::with_id(app, "show", "Show Delta", true, None::<&str>)?;
            let hide_item = MenuItem::with_id(app, "hide", "Hide", true, None::<&str>)?;
            let sign_in_item = MenuItem::with_id(
                app,
                "sign-in-browser",
                "Sign in in browser",
                true,
                None::<&str>,
            )?;
            let notify_item = MenuItem::with_id(
                app,
                "test-notification",
                "Test notification",
                true,
                None::<&str>,
            )?;
            let update_item =
                MenuItem::with_id(app, "check-updates", "Check for updates", true, None::<&str>)?;
            apply_autostart_default(&handle);
            let autostart_item = CheckMenuItem::with_id(
                app,
                "autostart",
                "Start with Windows",
                true,
                app.autolaunch().is_enabled().unwrap_or(false),
                None::<&str>,
            )?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(
                app,
                &[
                    &show_item,
                    &hide_item,
                    &sign_in_item,
                    &notify_item,
                    &update_item,
                    &autostart_item,
                    &quit_item,
                ],
            )?;

            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().cloned().expect("app icon"))
                .menu(&menu)
                .show_menu_on_left_click(false)
                .tooltip("Delta")
                .on_menu_event(move |app, event| match event.id.as_ref() {
                    "show" => show_main_window(app),
                    "hide" => hide_main_window(app),
                    "sign-in-browser" => {
                        // Until Dalton approves the /desktop/auth/* web routes,
                        // this opens the existing Clerk sign-in page in a real
                        // browser so Google OAuth is not blocked by WebView2.
                        open_external(app, &format!("{}/sign-in", app_origin()));
                    }
                    "test-notification" => {
                        notify(app, "Delta", "Native notifications are working.");
                    }
                    "check-updates" => manual_update(app.clone()),
                    "autostart" => toggle_autostart(app, &autostart_item),
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
                })
                .build(app)?;

            app.deep_link().on_open_url({
                let handle = handle.clone();
                move |event| {
                    for url in event.urls() {
                        handle_deep_link(&handle, url.as_str());
                    }
                }
            });

            if let Ok(Some(urls)) = app.deep_link().get_current() {
                for url in urls {
                    handle_deep_link(&handle, url.as_str());
                }
            }

            // The window starts hidden (tauri.conf.json) so a login launch, or a
            // relaunch after a background update, can stay in the tray without a
            // flash. Show it for normal launches.
            let stay_hidden = take_start_hidden_marker(&handle) || launched_by_autostart();
            if !stay_hidden {
                show_main_window(&handle);
            }

            announce_new_version(&handle);
            start_auto_updater(handle);
            Ok(())
        })
        .on_window_event(|window, event| match event {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                let _ = window.hide();
            }
            tauri::WindowEvent::Focused(focused) => record_focus(*focused),
            _ => {}
        })
        .run(tauri::generate_context!())
        .expect("error while running Delta desktop");
}
