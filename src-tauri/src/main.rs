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
    AppHandle, CustomMenuItem, Manager, SystemTray, SystemTrayEvent, SystemTrayMenu,
    SystemTrayMenuItem, WindowEvent,
};

use commands::{
    clear_all_notifications, clear_app_notifications, clear_notification,
    get_notification_groups, inject_dummy_notifications, summarize_notifications,
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

pub(crate) fn emit_notifications_updated(app: &AppHandle) {
    if let Err(err) = app.emit_all("notifications-updated", ()) {
        warn!("failed to emit notifications-updated: {err}");
    }
}

fn toggle_main_window(app: &AppHandle) {
    let Some(window) = app.get_window("main") else {
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
            emit_notifications_updated(app);
        }
        Err(err) => {
            warn!("failed to read window visibility: {err}");
        }
    }
}

fn start_polling_thread(app: AppHandle, orchestrator: Arc<Mutex<NotifyOrchestrator>>) {
    thread::spawn(move || loop {
        let changed = {
            let mut guard = match orchestrator.lock() {
                Ok(guard) => guard,
                Err(err) => {
                    error!("Orchestrator lock poisoned: {err}");
                    thread::sleep(Duration::from_secs(POLL_INTERVAL_SECONDS));
                    continue;
                }
            };
            guard.poll()
        };

        if changed {
            emit_notifications_updated(&app);
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
                .map(|mut guard| guard.clear_all())
                .unwrap_or(0);
            if cleared > 0 {
                emit_notifications_updated(app);
                show_notification("通知クリア", &format!("{}件を削除しました", cleared));
            }
        }
        "summarize" => {
            let state = app.state::<SharedOrchestrator>();
            let summary = state
                .0
                .lock()
                .ok()
                .and_then(|guard| guard.summarize_collected());
            match summary {
                Some(text) => show_dialog("通知まとめ", &text),
                None => show_notification("通知なし", "収集済みの通知はありません。"),
            }
        }
        _ => {}
    }
}

fn tray() -> SystemTray {
    let summarize_item = CustomMenuItem::new("summarize".to_string(), "通知を要約");
    let clear_item = CustomMenuItem::new("clear_all".to_string(), "全通知をクリア");
    let quit_item = CustomMenuItem::new("quit".to_string(), "終了");

    let menu = SystemTrayMenu::new()
        .add_item(summarize_item)
        .add_item(clear_item)
        .add_native_item(SystemTrayMenuItem::Separator)
        .add_item(quit_item);

    let tray = SystemTray::new().with_menu(menu);
    #[cfg(target_os = "macos")]
    let tray = tray.with_menu_on_left_click(false);

    tray
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
            summarize_notifications
        ])
        .system_tray(tray())
        .on_window_event(|event| {
            if event.window().label() == "main" {
                if let WindowEvent::Focused(false) = event.event() {
                    let _ = event.window().hide();
                }
            }
        })
        .on_system_tray_event(|app, event| match event {
            SystemTrayEvent::LeftClick { .. } => toggle_main_window(app),
            SystemTrayEvent::MenuItemClick { id, .. } => handle_tray_menu_event(app, &id),
            _ => {}
        })
        .setup(|app| {
            if let Some(window) = app.get_window("main") {
                let _ = window.hide();
                let _ = window.set_always_on_top(true);
            }
            let orchestrator = app.state::<SharedOrchestrator>().0.clone();
            start_polling_thread(app.handle(), orchestrator);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
