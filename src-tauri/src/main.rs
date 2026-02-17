#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod db;
mod focus;
mod llm;
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
    inject_dummy_notifications, open_app, remove_ignored_app, set_app_prompt,
};
use llm::LlmClient;
use orchestrator::{
    analyze_notifications_batch, NotifyOrchestrator, SharedOrchestrator, POLL_INTERVAL_SECONDS,
};

pub(crate) fn show_notification(title: &str, message: &str) {
    let escaped_title = escape_applescript(title);
    let escaped_message = escape_applescript(message);
    let script = format!(
        "display notification \"{}\" with title \"{}\"",
        escaped_message, escaped_title
    );
    run_osascript(&script);
}

pub(crate) fn show_dialog(title: &str, message: &str) -> Option<String> {
    let escaped_title = escape_applescript(title);
    let escaped_message = escape_applescript(message);
    let script = format!(
        "display dialog \"{}\" with title \"{}\" buttons {{\"OK\", \"アプリを開く\"}} default button \"OK\"",
        escaped_message, escaped_title
    );
    let result = Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(&script)
        .output();

    match result {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if stdout.contains("アプリを開く") {
                Some("open_app".to_string())
            } else {
                None
            }
        }
        Err(err) => {
            warn!("Failed to run osascript: {err}");
            None
        }
    }
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

fn highest_urgency_index(counts: [usize; 4]) -> Option<usize> {
    // counts: [critical, high, medium, low]
    counts.iter().position(|&c| c > 0)
}

fn update_tray(app: &AppHandle, counts: [usize; 4]) {
    let total: usize = counts.iter().sum();
    let title = if total == 0 {
        String::new()
    } else {
        format!("{total}")
    };

    #[cfg(target_os = "macos")]
    if let Some(state) = app.try_state::<TrayState>() {
        if let Err(err) = state.0.set_title(Some(&title)) {
            warn!("failed to update tray title: {err}");
        }

        let (icon, as_template) = match highest_urgency_index(counts) {
            Some(0) => (tauri::include_image!("icons/tray-critical.png"), false),
            Some(1) => (tauri::include_image!("icons/tray-high.png"), false),
            Some(2) => (tauri::include_image!("icons/tray-medium.png"), false),
            Some(3) => (tauri::include_image!("icons/tray-low.png"), false),
            _ => (tauri::include_image!("icons/tray.png"), true),
        };

        if let Err(err) = state.0.set_icon(Some(icon)) {
            warn!("failed to update tray icon: {err}");
        }
        if let Err(err) = state.0.set_icon_as_template(as_template) {
            warn!("failed to set icon template mode: {err}");
        }
    }
}

pub(crate) fn emit_notifications_updated(app: &AppHandle, counts: [usize; 4]) {
    if let Err(err) = app.emit("notifications-updated", ()) {
        warn!("failed to emit notifications-updated: {err}");
    }
    update_tray(app, counts);
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

fn start_polling_thread(
    app: AppHandle,
    orchestrator: Arc<Mutex<NotifyOrchestrator>>,
    llm: Arc<LlmClient>,
) {
    thread::spawn(move || loop {
        // Phase 1: Lock → DB read + filter → Unlock (fast, sub-millisecond)
        let poll_result = {
            let mut guard = match orchestrator.lock() {
                Ok(guard) => guard,
                Err(err) => {
                    error!("Orchestrator lock poisoned: {err}");
                    thread::sleep(Duration::from_secs(POLL_INTERVAL_SECONDS));
                    continue;
                }
            };
            guard.poll_read_new()
        };

        // Phase 2: LLM analysis (NO lock held, may take seconds/minutes)
        let (analyzed, criticals) = if poll_result.pending.is_empty() {
            (Vec::new(), Vec::new())
        } else {
            analyze_notifications_batch(&llm, poll_result.pending)
        };

        // Phase 3: Lock → store results → Unlock (fast)
        let counts = {
            let mut guard = match orchestrator.lock() {
                Ok(guard) => guard,
                Err(err) => {
                    error!("Orchestrator lock poisoned: {err}");
                    thread::sleep(Duration::from_secs(POLL_INTERVAL_SECONDS));
                    continue;
                }
            };
            let changed = guard.poll_store_results(analyzed);
            if poll_result.focus_ended {
                guard.on_focus_ended();
            }
            if changed || poll_result.focus_ended {
                Some(guard.urgency_counts())
            } else {
                None
            }
        };

        if let Some(counts) = counts {
            emit_notifications_updated(&app, counts);
        }

        // Phase 4: Show critical dialogs (NO lock held, may block on user input)
        for critical in &criticals {
            let result = show_dialog(
                "緊急通知",
                &format!("{}\n{}", critical.title, critical.body),
            );
            if result.as_deref() == Some("open_app") {
                if let Err(err) = std::process::Command::new("open")
                    .arg("-b")
                    .arg(&critical.bundle_id)
                    .spawn()
                {
                    warn!("failed to open app {}: {err}", critical.bundle_id);
                }
            }
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

    let llm = Arc::new(LlmClient::new());

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
            remove_ignored_app,
            open_app
        ])
        .on_window_event(|window, event| {
            if window.label() == "main" {
                if let WindowEvent::Focused(false) = event {
                    let _ = window.hide();
                }
            }
        })
        .setup(move |app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let tray = setup_tray(app)?;
            app.manage(TrayState(tray));

            if let Some(window) = app.get_webview_window("main") {
                let _ = window.hide();
                let _ = window.set_always_on_top(true);
            }
            let orchestrator = app.state::<SharedOrchestrator>().0.clone();
            start_polling_thread(app.handle().clone(), orchestrator, llm.clone());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
