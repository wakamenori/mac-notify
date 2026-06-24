use std::env;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use log::warn;
use serde_json::Value;

use crate::app_log;
use crate::models::FocusState;

static LAST_FOCUS_ERROR: OnceLock<Mutex<Option<String>>> = OnceLock::new();

pub struct FocusModeDetector {
    assertions_path: PathBuf,
}

impl FocusModeDetector {
    pub fn new(assertions_path: PathBuf) -> Self {
        Self { assertions_path }
    }

    pub fn get_state(&self) -> FocusState {
        let text = match std::fs::read_to_string(&self.assertions_path) {
            Ok(text) => text,
            Err(err) => {
                warn!(
                    "Cannot read focus assertions: {} ({})",
                    self.assertions_path.display(),
                    err
                );
                log_focus_error_once(format!(
                    "focus assertions read failed path={} error={err}",
                    self.assertions_path.display()
                ));
                return FocusState::Inactive;
            }
        };

        let data: Value = match serde_json::from_str(&text) {
            Ok(data) => data,
            Err(err) => {
                warn!(
                    "Cannot parse focus assertions JSON: {} ({})",
                    self.assertions_path.display(),
                    err
                );
                log_focus_error_once(format!(
                    "focus assertions parse failed path={} error={err}",
                    self.assertions_path.display()
                ));
                return FocusState::Inactive;
            }
        };

        if is_focus_active(&data) {
            FocusState::Active
        } else {
            FocusState::Inactive
        }
    }
}

fn log_focus_error_once(message: String) {
    let lock = LAST_FOCUS_ERROR.get_or_init(|| Mutex::new(None));
    let Ok(mut last) = lock.lock() else {
        app_log::warn(message);
        return;
    };
    if last.as_deref() == Some(message.as_str()) {
        return;
    }
    *last = Some(message.clone());
    app_log::warn(message);
}

fn is_focus_active(data: &Value) -> bool {
    data.get("data")
        .and_then(Value::as_array)
        .map(|records| {
            records.iter().any(|record| {
                record
                    .as_object()
                    .and_then(|obj| obj.get("storeAssertionRecords"))
                    .is_some_and(|v| !v.is_null() && v != &Value::Bool(false))
            })
        })
        .unwrap_or(false)
}

pub fn get_focus_assertions_path() -> PathBuf {
    let home = env::var("HOME").unwrap_or_default();
    let primary = PathBuf::from(home)
        .join("Library")
        .join("DoNotDisturb")
        .join("DB")
        .join("Assertions.json");

    if primary.exists() {
        return primary;
    }

    PathBuf::from("/Users/Shared/.FocusConfiguration/Assertions.json")
}
