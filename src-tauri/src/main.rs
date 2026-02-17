#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod db;
mod focus;
mod gemini;
mod models;
mod orchestrator;

use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use log::{error, warn};
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, WindowEvent,
};

use commands::{
    add_ignored_app, clear_all_notifications, clear_app_notifications, clear_notification,
    delete_app_prompt, get_app_prompts, get_ignored_apps, get_notification_groups,
    inject_dummy_notifications, remove_ignored_app, set_app_prompt,
};
use orchestrator::{NotifyOrchestrator, SharedOrchestrator, POLL_INTERVAL_SECONDS};

pub(crate) fn show_notification(title: &str, message: &str) {
    let escaped_title = escape_applescript(title);
    let escaped_message = escape_applescript(message);
    let script = format!(
        "display notification \"{}\" with title \"{}\"",
        escaped_message, escaped_title
    );
    run_osascript(&script);
}

pub(crate) fn show_dialog(title: &str, message: &str) {
    let escaped_title = escape_applescript(title);
    let escaped_message = escape_applescript(message);
    let script = format!(
        "display dialog \"{}\" with title \"{}\" buttons {{\"OK\"}} default button \"OK\"",
        escaped_message, escaped_title
    );
    run_osascript(&script);
}

fn escape_applescript(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

fn run_osascript(script: &str) {
    let result = Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(script)
        .output();

    if let Err(err) = result {
        warn!("Failed to run osascript: {err}");
    }
}

struct TrayState(tauri::tray::TrayIcon);

fn update_tray_title(app: &AppHandle, counts: [usize; 4]) {
    let labels = ["R4", "R3", "R2", "R1"];
    let parts: Vec<String> = counts
        .iter()
        .zip(labels.iter())
        .filter(|(c, _)| **c > 0)
        .map(|(c, l)| format!("{l}:{c}"))
        .collect();
    let title = if parts.is_empty() {
        String::new()
    } else {
        parts.join(" ")
    };
    #[cfg(target_os = "macos")]
    if let Some(state) = app.try_state::<TrayState>() {
        if let Err(err) = state.0.set_title(Some(&title)) {
            warn!("failed to update tray title: {err}");
        }
    }
}

pub(crate) fn emit_notifications_updated(app: &AppHandle, counts: [usize; 4]) {
    if let Err(err) = app.emit("notifications-updated", ()) {
        warn!("failed to emit notifications-updated: {err}");
    }
    update_tray_title(app, counts);
}

fn toggle_main_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        warn!("main window not found");
        return;
    };

    match window.is_visible() {
        Ok(true) => {
            if let Err(err) = window.hide() {
                warn!("failed to hide window: {err}");
            }
        }
        Ok(false) => {
            if let Err(err) = window.show() {
                warn!("failed to show window: {err}");
                return;
            }
            let _ = window.unminimize();
            let _ = window.set_focus();
            let counts = app
                .state::<SharedOrchestrator>()
                .0
                .lock()
                .map(|guard| guard.urgency_counts())
                .unwrap_or([0; 4]);
            emit_notifications_updated(app, counts);
        }
        Err(err) => {
            warn!("failed to read window visibility: {err}");
        }
    }
}

fn start_polling_thread(app: AppHandle, orchestrator: Arc<Mutex<NotifyOrchestrator>>) {
    thread::spawn(move || loop {
        let result = {
            let mut guard = match orchestrator.lock() {
                Ok(guard) => guard,
                Err(err) => {
                    error!("Orchestrator lock poisoned: {err}");
                    thread::sleep(Duration::from_secs(POLL_INTERVAL_SECONDS));
                    continue;
                }
            };
            let changed = guard.poll();
            if changed {
                Some(guard.urgency_counts())
            } else {
                None
            }
        };

        if let Some(counts) = result {
            emit_notifications_updated(&app, counts);
        }

        thread::sleep(Duration::from_secs(POLL_INTERVAL_SECONDS));
    });
}

fn handle_tray_menu_event(app: &AppHandle, id: &str) {
    match id {
        "quit" => {
            app.exit(0);
        }
        "clear_all" => {
            let state = app.state::<SharedOrchestrator>();
            let cleared = state
                .0
                .lock()
                .ok()
                .map(|mut guard| {
                    let c = guard.clear_all();
                    (c, guard.urgency_counts())
                })
                .unwrap_or((0, [0; 4]));
            if cleared.0 > 0 {
                emit_notifications_updated(app, cleared.1);
                show_notification("通知クリア", &format!("{}件を削除しました", cleared.0));
            }
        }
        _ => {}
    }
}

fn setup_tray(app: &tauri::App) -> Result<tauri::tray::TrayIcon, Box<dyn std::error::Error>> {
    let clear_item = MenuItem::with_id(app, "clear_all", "全通知をクリア", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit_item = MenuItem::with_id(app, "quit", "終了", true, None::<&str>)?;

    let menu = Menu::with_items(app, &[&clear_item, &separator, &quit_item])?;

    let tray = TrayIconBuilder::new()
        .menu(&menu)
        .show_menu_on_left_click(false)
        .icon(tauri::include_image!("icons/tray.png"))
        .icon_as_template(true)
        .on_menu_event(|app, event| {
            handle_tray_menu_event(app, event.id().as_ref());
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_main_window(tray.app_handle());
            }
        })
        .build(app)?;

    Ok(tray)
}

fn main() {
    dotenvy::dotenv().ok();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let orchestrator = match NotifyOrchestrator::new() {
        Ok(orchestrator) => Arc::new(Mutex::new(orchestrator)),
        Err(err) => {
            eprintln!("failed to initialize mac-notify: {err:#}");
            std::process::exit(1);
        }
    };

    tauri::Builder::default()
        .manage(SharedOrchestrator(orchestrator))
        .invoke_handler(tauri::generate_handler![
            get_notification_groups,
            clear_notification,
            clear_app_notifications,
            clear_all_notifications,
            inject_dummy_notifications,
            get_app_prompts,
            set_app_prompt,
            delete_app_prompt,
            get_ignored_apps,
            add_ignored_app,
            remove_ignored_app
        ])
        .on_window_event(|window, event| {
            if window.label() == "main" {
                if let WindowEvent::Focused(false) = event {
                    let _ = window.hide();
                }
            }
        })
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let tray = setup_tray(app)?;
            app.manage(TrayState(tray));

            if let Some(window) = app.get_webview_window("main") {
                let _ = window.hide();
                let _ = window.set_always_on_top(true);
            }
            let orchestrator = app.state::<SharedOrchestrator>().0.clone();
            start_polling_thread(app.handle().clone(), orchestrator);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
