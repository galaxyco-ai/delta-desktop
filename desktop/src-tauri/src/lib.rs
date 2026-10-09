//! Thin Windows shell around the Delta web app.
//!
//! Production loads https://delta.galaxyco.ai. Dev loads http://localhost:3000.
//! Only the canonical host is allowed. Do not point this at any other deployment.

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Manager, WebviewWindow};
use tauri_plugin_deep_link::DeepLinkExt;
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_updater::UpdaterExt;
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

fn check_for_updates(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        match app.updater() {
            Ok(updater) => match updater.check().await {
                Ok(Some(update)) => {
                    notify(
                        &app,
                        "Delta update available",
                        &format!("Version {} is ready. Use Check for updates in the tray to install.", update.version),
                    );
                }
                Ok(None) => {}
                Err(error) => eprintln!("delta-desktop: updater check failed: {error}"),
            },
            Err(error) => eprintln!("delta-desktop: updater unavailable: {error}"),
        }
    });
}

fn install_update(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        let updater = match app.updater() {
            Ok(updater) => updater,
            Err(error) => {
                notify(&app, "Delta", &format!("Updater unavailable: {error}"));
                return;
            }
        };
        match updater.check().await {
            Ok(Some(update)) => {
                notify(
                    &app,
                    "Delta",
                    &format!("Downloading version {}.", update.version),
                );
                if let Err(error) = update.download_and_install(|_, _| {}, || {}).await {
                    notify(&app, "Delta", &format!("Update failed: {error}"));
                    return;
                }
                notify(&app, "Delta", "Update installed. Restarting.");
                app.restart();
            }
            Ok(None) => notify(&app, "Delta", "You are on the latest desktop build."),
            Err(error) => notify(&app, "Delta", &format!("Update check failed: {error}")),
        }
    });
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
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
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
            let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(
                app,
                &[
                    &show_item,
                    &hide_item,
                    &sign_in_item,
                    &notify_item,
                    &update_item,
                    &quit_item,
                ],
            )?;

            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().cloned().expect("app icon"))
                .menu(&menu)
                .show_menu_on_left_click(false)
                .tooltip("Delta")
                .on_menu_event(|app, event| match event.id.as_ref() {
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
                    "check-updates" => install_update(app.clone()),
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

            check_for_updates(handle);
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running Delta desktop");
}
