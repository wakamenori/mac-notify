use serde::Serialize;
use tauri::{AppHandle, State};

use crate::emit_notifications_updated;
use crate::models::UiNotificationGroup;
use crate::orchestrator::{SharedOrchestrator, MAX_DUMMY_INSERT_COUNT};

#[derive(Serialize)]
pub struct AppPromptEntry {
    #[serde(rename = "bundleId")]
    pub bundle_id: String,
    pub context: String,
}

#[tauri::command]
pub fn get_notification_groups(
    state: State<'_, SharedOrchestrator>,
) -> Result<Vec<UiNotificationGroup>, String> {
    let guard = state
        .0
        .lock()
        .map_err(|err| format!("state lock error: {err}"))?;
    Ok(guard.notification_groups())
}

#[tauri::command]
pub fn clear_notification(
    id: i64,
    state: State<'_, SharedOrchestrator>,
    app: AppHandle,
) -> Result<bool, String> {
    let mut guard = state
        .0
        .lock()
        .map_err(|err| format!("state lock error: {err}"))?;
    let cleared = guard.clear_notification(id);
    if cleared {
        let counts = guard.urgency_counts();
        emit_notifications_updated(&app, counts);
    }
    Ok(cleared)
}

#[tauri::command]
pub fn clear_app_notifications(
    bundle_id: String,
    state: State<'_, SharedOrchestrator>,
    app: AppHandle,
) -> Result<usize, String> {
    let mut guard = state
        .0
        .lock()
        .map_err(|err| format!("state lock error: {err}"))?;
    let cleared = guard.clear_app_notifications(&bundle_id);
    if cleared > 0 {
        let counts = guard.urgency_counts();
        emit_notifications_updated(&app, counts);
    }
    Ok(cleared)
}

#[tauri::command]
pub fn clear_all_notifications(
    state: State<'_, SharedOrchestrator>,
    app: AppHandle,
) -> Result<usize, String> {
    let mut guard = state
        .0
        .lock()
        .map_err(|err| format!("state lock error: {err}"))?;
    let cleared = guard.clear_all();
    if cleared > 0 {
        let counts = guard.urgency_counts();
        emit_notifications_updated(&app, counts);
    }
    Ok(cleared)
}

#[tauri::command]
pub fn inject_dummy_notifications(
    count: Option<usize>,
    state: State<'_, SharedOrchestrator>,
    app: AppHandle,
) -> Result<usize, String> {
    let insert_count = count.unwrap_or(8).clamp(1, MAX_DUMMY_INSERT_COUNT);
    let mut guard = state
        .0
        .lock()
        .map_err(|err| format!("state lock error: {err}"))?;
    let inserted = guard.inject_dummy_notifications(insert_count);
    let counts = guard.urgency_counts();
    emit_notifications_updated(&app, counts);
    Ok(inserted)
}

#[tauri::command]
pub fn get_app_prompts(
    state: State<'_, SharedOrchestrator>,
) -> Result<Vec<AppPromptEntry>, String> {
    let guard = state
        .0
        .lock()
        .map_err(|err| format!("state lock error: {err}"))?;
    let entries = guard
        .list_app_prompts()
        .into_iter()
        .map(|(bundle_id, context)| AppPromptEntry { bundle_id, context })
        .collect();
    Ok(entries)
}

#[tauri::command]
pub fn set_app_prompt(
    bundle_id: String,
    context: String,
    state: State<'_, SharedOrchestrator>,
) -> Result<(), String> {
    let mut guard = state
        .0
        .lock()
        .map_err(|err| format!("state lock error: {err}"))?;
    guard
        .set_app_prompt(bundle_id, context)
        .map_err(|err| format!("failed to save app prompt: {err}"))
}

#[tauri::command]
pub fn open_app(bundle_id: String) -> Result<(), String> {
    log::info!("open_app called with bundle_id: {bundle_id}");
    std::process::Command::new("open")
        .arg("-b")
        .arg(&bundle_id)
        .spawn()
        .map_err(|err| format!("failed to open app {bundle_id}: {err}"))?;
    Ok(())
}

#[tauri::command]
pub fn delete_app_prompt(
    bundle_id: String,
    state: State<'_, SharedOrchestrator>,
) -> Result<bool, String> {
    let mut guard = state
        .0
        .lock()
        .map_err(|err| format!("state lock error: {err}"))?;
    guard
        .delete_app_prompt(&bundle_id)
        .map_err(|err| format!("failed to delete app prompt: {err}"))
}

#[tauri::command]
pub fn get_ignored_apps(state: State<'_, SharedOrchestrator>) -> Result<Vec<String>, String> {
    let guard = state
        .0
        .lock()
        .map_err(|err| format!("state lock error: {err}"))?;
    Ok(guard.list_ignored_apps())
}

#[tauri::command]
pub fn add_ignored_app(
    bundle_id: String,
    state: State<'_, SharedOrchestrator>,
) -> Result<(), String> {
    let mut guard = state
        .0
        .lock()
        .map_err(|err| format!("state lock error: {err}"))?;
    guard
        .add_ignored_app(bundle_id)
        .map_err(|err| format!("failed to save ignored app: {err}"))
}

#[tauri::command]
pub fn remove_ignored_app(
    bundle_id: String,
    state: State<'_, SharedOrchestrator>,
) -> Result<bool, String> {
    let mut guard = state
        .0
        .lock()
        .map_err(|err| format!("state lock error: {err}"))?;
    guard
        .remove_ignored_app(&bundle_id)
        .map_err(|err| format!("failed to remove ignored app: {err}"))
}
