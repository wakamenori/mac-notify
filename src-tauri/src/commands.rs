use tauri::{AppHandle, State};

use crate::models::UiNotificationGroup;
use crate::orchestrator::{SharedOrchestrator, MAX_DUMMY_INSERT_COUNT};
use crate::emit_notifications_updated;

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
        emit_notifications_updated(&app);
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
        emit_notifications_updated(&app);
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
        emit_notifications_updated(&app);
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
    emit_notifications_updated(&app);
    Ok(inserted)
}

#[tauri::command]
pub fn summarize_notifications(state: State<'_, SharedOrchestrator>) -> Result<String, String> {
    let guard = state
        .0
        .lock()
        .map_err(|err| format!("state lock error: {err}"))?;
    guard
        .summarize_collected()
        .ok_or_else(|| "収集済み通知はありません。".to_string())
}
