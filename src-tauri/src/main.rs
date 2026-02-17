#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::BTreeMap;
use std::env;
use std::io::Cursor;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use log::{error, warn};
use plist::Value as PlistValue;
use reqwest::blocking::Client;
use rusqlite::{params, Connection, OpenFlags};
use serde_json::{json, Value};
use tauri::{
    AppHandle, CustomMenuItem, Manager, SystemTray, SystemTrayEvent, SystemTrayMenu,
    SystemTrayMenuItem,
};

const POLL_INTERVAL_SECONDS: u64 = 5;
const GEMINI_MODEL: &str = "gemini-2.5-flash-lite";

const SCHEMA_QUERY_Z: &str = "SELECT rec.Z_PK, rec.ZDATA, app.ZBUNDLEID \
FROM ZNOTIFICATIONENTRY rec \
JOIN ZNOTIFICATIONAPPENTRY app ON rec.ZAPP = app.Z_PK \
WHERE rec.Z_PK > ? \
ORDER BY rec.Z_PK";

const SCHEMA_QUERY_RECORD: &str = "SELECT rec.rec_id, rec.data, app.identifier \
FROM record rec \
JOIN app ON rec.app_id = app.app_id \
WHERE rec.rec_id > ? \
ORDER BY rec.rec_id";

const SCHEMA_MAX_ROWID_Z: &str = "SELECT MAX(Z_PK) FROM ZNOTIFICATIONENTRY";
const SCHEMA_MAX_ROWID_RECORD: &str = "SELECT MAX(rec_id) FROM record";

#[derive(Clone)]
struct SharedOrchestrator(Arc<Mutex<NotifyOrchestrator>>);

#[derive(Debug, Clone)]
struct Notification {
    rowid: i64,
    title: String,
    body: String,
    subtitle: String,
    bundle_id: String,
}

#[derive(Debug, Clone)]
struct NotificationSummary {
    text: String,
    notification_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UrgencyLevel {
    Normal,
    Urgent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusState {
    Active,
    Inactive,
}

#[derive(Debug, Clone)]
struct ParsedPlist {
    title: String,
    body: String,
    subtitle: String,
}

struct NotificationDb {
    db_path: PathBuf,
    query: Option<&'static str>,
}

impl NotificationDb {
    fn new(db_path: PathBuf) -> Self {
        Self {
            db_path,
            query: None,
        }
    }

    fn read_new(&mut self, since_rowid: i64) -> Result<Vec<Notification>> {
        let conn = Connection::open_with_flags(&self.db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("cannot open notification DB: {}", self.db_path.display()))?;

        let query = self.resolve_query(&conn)?;
        let mut statement = conn.prepare(query)?;
        let rows = statement.query_map(params![since_rowid], |row| {
            let rowid: i64 = row.get(0)?;
            let data: Vec<u8> = row.get(1)?;
            let bundle_id: String = row.get(2)?;
            Ok((rowid, data, bundle_id))
        })?;

        let mut notifications = Vec::new();
        for row in rows {
            let (rowid, data, bundle_id) = row?;
            let parsed = parse_notification_plist(&data);

            notifications.push(Notification {
                rowid,
                title: parsed.title,
                body: parsed.body,
                subtitle: parsed.subtitle,
                bundle_id,
            });
        }

        Ok(notifications)
    }

    fn latest_rowid(&mut self) -> Result<i64> {
        let conn = Connection::open_with_flags(&self.db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("cannot open notification DB: {}", self.db_path.display()))?;

        let query = self.resolve_query(&conn)?;
        let max_query = match query {
            SCHEMA_QUERY_Z => SCHEMA_MAX_ROWID_Z,
            SCHEMA_QUERY_RECORD => SCHEMA_MAX_ROWID_RECORD,
            _ => bail!("unsupported schema query"),
        };

        let mut statement = conn.prepare(max_query)?;
        let max_rowid = statement.query_row([], |row| row.get::<_, Option<i64>>(0))?;
        Ok(max_rowid.unwrap_or(0))
    }

    fn resolve_query(&mut self, conn: &Connection) -> Result<&'static str> {
        if let Some(query) = self.query {
            return Ok(query);
        }

        for query in [SCHEMA_QUERY_Z, SCHEMA_QUERY_RECORD] {
            if let Ok(mut statement) = conn.prepare(query) {
                if statement.query(params![0]).is_ok() {
                    self.query = Some(query);
                    return Ok(query);
                }
            }
        }

        bail!("could not determine notification DB schema")
    }
}

struct FocusModeDetector {
    assertions_path: PathBuf,
}

impl FocusModeDetector {
    fn new(assertions_path: PathBuf) -> Self {
        Self { assertions_path }
    }

    fn get_state(&self) -> FocusState {
        let text = match std::fs::read_to_string(&self.assertions_path) {
            Ok(text) => text,
            Err(err) => {
                warn!(
                    "Cannot read focus assertions: {} ({})",
                    self.assertions_path.display(),
                    err
                );
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

struct GeminiClient {
    api_key: String,
    client: Client,
}

impl GeminiClient {
    fn new(api_key: String) -> Self {
        Self {
            api_key,
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("failed to build reqwest client"),
        }
    }

    fn can_use(&self) -> bool {
        !self.api_key.is_empty()
    }

    fn generate_text(&self, prompt: &str) -> Result<String> {
        if !self.can_use() {
            bail!("GOOGLE_API_KEY is not set")
        }

        let endpoint = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
            GEMINI_MODEL, self.api_key
        );

        let response: Value = self
            .client
            .post(endpoint)
            .json(&json!({
                "contents": [
                    {
                        "parts": [{ "text": prompt }]
                    }
                ]
            }))
            .send()?
            .error_for_status()?
            .json()?;

        let text = response
            .pointer("/candidates/0/content/parts/0/text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();

        if text.is_empty() {
            bail!("Gemini response text is empty")
        }

        Ok(text)
    }
}

struct NotifyOrchestrator {
    reader: NotificationDb,
    focus_detector: FocusModeDetector,
    gemini: GeminiClient,
    last_rowid: i64,
    collected: Vec<Notification>,
    was_focused: bool,
}

impl NotifyOrchestrator {
    fn new() -> Result<Self> {
        let db_path = get_notification_db_path()?;
        let assertions_path = get_focus_assertions_path();
        let google_api_key = env::var("GOOGLE_API_KEY").unwrap_or_default();
        let mut reader = NotificationDb::new(db_path);
        let initial_rowid = reader.latest_rowid()?;

        Ok(Self {
            reader,
            focus_detector: FocusModeDetector::new(assertions_path),
            gemini: GeminiClient::new(google_api_key),
            last_rowid: initial_rowid,
            collected: Vec::new(),
            was_focused: false,
        })
    }

    fn poll(&mut self) {
        let is_focused = self.focus_detector.get_state() == FocusState::Active;

        match self.reader.read_new(self.last_rowid) {
            Ok(new_notifications) => {
                if let Some(last) = new_notifications.last() {
                    self.last_rowid = last.rowid;
                }
                if is_focused {
                    self.handle_new_notifications(new_notifications);
                }
            }
            Err(err) => {
                error!("Error reading notification DB: {err:#}");
            }
        }

        if !is_focused && self.was_focused && !self.collected.is_empty() {
            self.on_focus_ended();
        }

        self.was_focused = is_focused;
    }

    fn handle_new_notifications(&mut self, notifications: Vec<Notification>) {
        for notification in notifications {
            if self.classify_urgency(&notification) == UrgencyLevel::Urgent {
                show_dialog(
                    "緊急通知",
                    &format!("{}\n{}", notification.title, notification.body),
                );
                continue;
            }
            self.collected.push(notification);
        }
    }

    fn classify_urgency(&self, notification: &Notification) -> UrgencyLevel {
        if !self.gemini.can_use() {
            return UrgencyLevel::Normal;
        }

        let prompt = format!(
            "以下の通知が緊急かどうかを判定してください。\n\
緊急の例: 電話の着信、セキュリティアラート、システム警告、災害通知\n\
通常の例: メール、チャット、アプリ更新、ニュース\n\n\
通知:\n\
アプリ: {}\n\
タイトル: {}\n\
本文: {}\n\n\
JSON で回答してください: {{\"urgency\": \"urgent\"}} または {{\"urgency\": \"normal\"}}",
            notification.bundle_id, notification.title, notification.body
        );

        let response_text = match self.gemini.generate_text(&prompt) {
            Ok(text) => text,
            Err(err) => {
                warn!("Urgency classification failed: {err:#}");
                return UrgencyLevel::Normal;
            }
        };

        parse_urgency_response(&response_text)
    }

    fn on_focus_ended(&mut self) {
        let summary = self.summarize(&self.collected);
        show_notification(
            "集中モード終了",
            &format!("{}件の通知があります", summary.notification_count),
        );
        show_dialog("通知まとめ", &summary.text);
        self.collected.clear();
    }

    fn summarize_collected(&self) -> Option<String> {
        if self.collected.is_empty() {
            return None;
        }
        Some(self.summarize(&self.collected).text)
    }

    fn summarize(&self, notifications: &[Notification]) -> NotificationSummary {
        if notifications.is_empty() {
            return NotificationSummary {
                text: "通知はありません。".to_string(),
                notification_count: 0,
            };
        }

        if self.gemini.can_use() {
            let prompt = build_summary_prompt(notifications);
            if let Ok(text) = self.gemini.generate_text(&prompt) {
                return NotificationSummary {
                    text,
                    notification_count: notifications.len(),
                };
            }
        }

        NotificationSummary {
            text: fallback_summary(notifications),
            notification_count: notifications.len(),
        }
    }

    fn collected_count(&self) -> usize {
        self.collected.len()
    }
}

fn build_summary_prompt(notifications: &[Notification]) -> String {
    let body = notifications
        .iter()
        .map(|n| {
            let mut line = format!("[{}]", n.bundle_id);
            if !n.title.is_empty() {
                line.push(' ');
                line.push_str(&n.title);
            }
            if !n.subtitle.is_empty() {
                line.push_str(" - ");
                line.push_str(&n.subtitle);
            }
            if !n.body.is_empty() {
                line.push_str(": ");
                line.push_str(&n.body);
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "以下の macOS 通知を日本語で簡潔に要約してください。\n\
アプリごとにグループ化し、重要な情報を優先してください。\n\n{}",
        body
    )
}

fn fallback_summary(notifications: &[Notification]) -> String {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for n in notifications {
        *counts.entry(n.bundle_id.clone()).or_default() += 1;
    }

    let details = counts
        .into_iter()
        .map(|(app, count)| format!("{app}: {count}件"))
        .collect::<Vec<_>>()
        .join("\n");

    format!("通知 {}件:\n{}", notifications.len(), details)
}

fn parse_urgency_response(text: &str) -> UrgencyLevel {
    let Some(start) = text.find('{') else {
        return UrgencyLevel::Normal;
    };
    let Some(end) = text.rfind('}') else {
        return UrgencyLevel::Normal;
    };
    if end < start {
        return UrgencyLevel::Normal;
    }

    let json_text = &text[start..=end];
    let parsed: Value = match serde_json::from_str(json_text) {
        Ok(value) => value,
        Err(_) => return UrgencyLevel::Normal,
    };

    match parsed.get("urgency").and_then(Value::as_str) {
        Some("urgent") => UrgencyLevel::Urgent,
        _ => UrgencyLevel::Normal,
    }
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

fn parse_notification_plist(data: &[u8]) -> ParsedPlist {
    let parsed = PlistValue::from_reader(Cursor::new(data));
    let Ok(value) = parsed else {
        warn!("Failed to parse plist data");
        return ParsedPlist {
            title: String::new(),
            body: String::new(),
            subtitle: String::new(),
        };
    };

    let title = extract_plist_string(&value, &["titl"]);
    let body = extract_plist_string(&value, &["body"]);
    let subtitle = extract_plist_string(&value, &["subt"]);

    ParsedPlist {
        title: if title.is_empty() {
            extract_plist_string(&value, &["req", "titl"])
        } else {
            title
        },
        body: if body.is_empty() {
            extract_plist_string(&value, &["req", "body"])
        } else {
            body
        },
        subtitle: if subtitle.is_empty() {
            extract_plist_string(&value, &["req", "subt"])
        } else {
            subtitle
        },
    }
}

fn extract_plist_string(value: &PlistValue, keys: &[&str]) -> String {
    let mut current = value;
    for key in keys {
        let Some(dict) = current.as_dictionary() else {
            return String::new();
        };
        let Some(next) = dict.get(key) else {
            return String::new();
        };
        current = next;
    }

    current
        .as_string()
        .map(ToString::to_string)
        .unwrap_or_default()
}

fn get_notification_db_path() -> Result<PathBuf> {
    let major = macos_major_version();
    if major < 15 {
        bail!("mac-notify supports macOS 15 (Tahoe) or newer only. detected major: {major}");
    }

    let home = env::var("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Group Containers")
        .join("group.com.apple.usernoted")
        .join("db2")
        .join("db"))
}

fn get_focus_assertions_path() -> PathBuf {
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

fn macos_major_version() -> u32 {
    let output = Command::new("sw_vers").arg("-productVersion").output();
    let Ok(output) = output else {
        return 0;
    };
    let version = String::from_utf8_lossy(&output.stdout);
    let major = version.trim().split('.').next().unwrap_or_default();
    major.parse::<u32>().unwrap_or(0)
}

fn show_notification(title: &str, message: &str) {
    let escaped_title = escape_applescript(title);
    let escaped_message = escape_applescript(message);
    let script = format!(
        "display notification \"{}\" with title \"{}\"",
        escaped_message, escaped_title
    );
    run_osascript(&script);
}

fn show_dialog(title: &str, message: &str) {
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

fn start_polling_thread(app: AppHandle, orchestrator: Arc<Mutex<NotifyOrchestrator>>) {
    thread::spawn(move || loop {
        let count = {
            let mut guard = match orchestrator.lock() {
                Ok(guard) => guard,
                Err(err) => {
                    error!("Orchestrator lock poisoned: {err}");
                    thread::sleep(Duration::from_secs(POLL_INTERVAL_SECONDS));
                    continue;
                }
            };
            guard.poll();
            guard.collected_count()
        };

        if let Err(err) = update_tray_count(&app, count) {
            warn!("Failed to update tray UI: {err}");
        }

        thread::sleep(Duration::from_secs(POLL_INTERVAL_SECONDS));
    });
}

fn update_tray_count(app: &AppHandle, count: usize) -> tauri::Result<()> {
    app.tray_handle()
        .get_item("count")
        .set_title(format!("収集済み: {count}件"))?;
    Ok(())
}

fn handle_tray_menu_event(app: &AppHandle, id: &str) {
    match id {
        "quit" => {
            app.exit(0);
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
    let count_item = CustomMenuItem::new("count".to_string(), "収集済み: 0件").disabled();
    let quit_item = CustomMenuItem::new("quit".to_string(), "終了");

    let menu = SystemTrayMenu::new()
        .add_item(summarize_item)
        .add_item(count_item)
        .add_native_item(SystemTrayMenuItem::Separator)
        .add_item(quit_item);

    SystemTray::new().with_menu(menu)
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
        .system_tray(tray())
        .on_system_tray_event(|app, event| {
            if let SystemTrayEvent::MenuItemClick { id, .. } = event {
                handle_tray_menu_event(app, &id);
            }
        })
        .setup(|app| {
            let orchestrator = app.state::<SharedOrchestrator>().0.clone();
            start_polling_thread(app.handle(), orchestrator);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
