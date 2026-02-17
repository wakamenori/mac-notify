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
use serde::Serialize;
use serde_json::{json, Value};
use tauri::{
    AppHandle, CustomMenuItem, Manager, State, SystemTray, SystemTrayEvent, SystemTrayMenu,
    SystemTrayMenuItem, WindowEvent,
};

const POLL_INTERVAL_SECONDS: u64 = 5;
const GEMINI_MODEL: &str = "gemini-2.5-flash-lite";
const MAX_NOTIFICATIONS_PER_APP: usize = 12;
const MAX_DUMMY_INSERT_COUNT: usize = 30;

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
struct AnalyzedNotification {
    id: i64,
    title: String,
    body: String,
    subtitle: String,
    bundle_id: String,
    app_name: String,
    urgency: UrgencyLevel,
    summary_line: String,
    reason: String,
}

#[derive(Debug, Clone)]
struct NotificationAnalysis {
    urgency: UrgencyLevel,
    summary_line: String,
    reason: String,
}

#[derive(Debug, Clone)]
struct NotificationSummary {
    text: String,
    notification_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum UrgencyLevel {
    Critical,
    High,
    Medium,
    Low,
}

impl UrgencyLevel {
    fn label(self) -> &'static str {
        match self {
            Self::Critical => "R4",
            Self::High => "R3",
            Self::Medium => "R2",
            Self::Low => "R1",
        }
    }

    fn color(self) -> &'static str {
        match self {
            Self::Critical => "#ef4444",
            Self::High => "#f97316",
            Self::Medium => "#f59e0b",
            Self::Low => "#22c55e",
        }
    }
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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct UiNotification {
    id: i64,
    title: String,
    body: String,
    subtitle: String,
    bundle_id: String,
    app_name: String,
    urgency_level: UrgencyLevel,
    urgency_label: String,
    urgency_color: String,
    summary_line: String,
    reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct UiNotificationGroup {
    bundle_id: String,
    app_name: String,
    notifications: Vec<UiNotification>,
    hidden_count: usize,
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
    collected: Vec<AnalyzedNotification>,
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

    fn poll(&mut self) -> bool {
        let is_focused = self.focus_detector.get_state() == FocusState::Active;
        let mut changed = false;

        match self.reader.read_new(self.last_rowid) {
            Ok(new_notifications) => {
                if let Some(last) = new_notifications.last() {
                    self.last_rowid = last.rowid;
                }
                if is_focused {
                    changed = self.handle_new_notifications(new_notifications) || changed;
                }
            }
            Err(err) => {
                error!("Error reading notification DB: {err:#}");
            }
        }

        if !is_focused && self.was_focused && !self.collected.is_empty() {
            self.on_focus_ended();
            changed = true;
        }

        self.was_focused = is_focused;
        changed
    }

    fn handle_new_notifications(&mut self, notifications: Vec<Notification>) -> bool {
        let mut changed = false;

        for notification in notifications {
            let analysis = self.analyze_notification(&notification);
            if analysis.urgency == UrgencyLevel::Critical {
                show_dialog(
                    "緊急通知",
                    &format!("{}\n{}", notification.title, notification.body),
                );
            }

            self.collected.push(AnalyzedNotification {
                id: notification.rowid,
                title: notification.title,
                body: notification.body,
                subtitle: notification.subtitle,
                bundle_id: notification.bundle_id.clone(),
                app_name: app_name_from_bundle(&notification.bundle_id),
                urgency: analysis.urgency,
                summary_line: analysis.summary_line,
                reason: analysis.reason,
            });
            changed = true;
        }

        changed
    }

    fn analyze_notification(&self, notification: &Notification) -> NotificationAnalysis {
        if self.gemini.can_use() {
            let prompt = build_analysis_prompt(notification);
            match self.gemini.generate_text(&prompt) {
                Ok(text) => match parse_analysis_response(&text, notification) {
                    Some(parsed) => return parsed,
                    None => warn!("analysis response parse failed for {}", notification.rowid),
                },
                Err(err) => warn!("notification analysis failed: {err:#}"),
            }
        }

        fallback_analysis(notification)
    }

    fn on_focus_ended(&mut self) {
        let summary = self.summarize(&self.collected);
        show_notification(
            "集中モード終了",
            &format!("{}件の通知があります", summary.notification_count),
        );
        show_dialog("通知まとめ", &summary.text);
    }

    fn summarize_collected(&self) -> Option<String> {
        if self.collected.is_empty() {
            return None;
        }
        Some(self.summarize(&self.collected).text)
    }

    fn summarize(&self, notifications: &[AnalyzedNotification]) -> NotificationSummary {
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

    fn notification_groups(&self) -> Vec<UiNotificationGroup> {
        let mut grouped: BTreeMap<String, Vec<UiNotification>> = BTreeMap::new();

        for item in self.collected.iter().rev() {
            let entry = grouped.entry(item.bundle_id.clone()).or_default();
            entry.push(UiNotification {
                id: item.id,
                title: item.title.clone(),
                body: item.body.clone(),
                subtitle: item.subtitle.clone(),
                bundle_id: item.bundle_id.clone(),
                app_name: item.app_name.clone(),
                urgency_level: item.urgency,
                urgency_label: item.urgency.label().to_string(),
                urgency_color: item.urgency.color().to_string(),
                summary_line: item.summary_line.clone(),
                reason: item.reason.clone(),
            });
        }

        grouped
            .into_iter()
            .map(|(bundle_id, mut notifications)| {
                let app_name = notifications
                    .first()
                    .map(|n| n.app_name.clone())
                    .unwrap_or_else(|| app_name_from_bundle(&bundle_id));
                let hidden_count = notifications
                    .len()
                    .saturating_sub(MAX_NOTIFICATIONS_PER_APP);
                notifications.truncate(MAX_NOTIFICATIONS_PER_APP);

                UiNotificationGroup {
                    bundle_id,
                    app_name,
                    notifications,
                    hidden_count,
                }
            })
            .collect()
    }

    fn clear_notification(&mut self, id: i64) -> bool {
        let before = self.collected.len();
        self.collected.retain(|n| n.id != id);
        self.collected.len() != before
    }

    fn clear_app_notifications(&mut self, bundle_id: &str) -> usize {
        let before = self.collected.len();
        self.collected.retain(|n| n.bundle_id != bundle_id);
        before.saturating_sub(self.collected.len())
    }

    fn clear_all(&mut self) -> usize {
        let count = self.collected.len();
        self.collected.clear();
        count
    }

    fn inject_dummy_notifications(&mut self, count: usize) -> usize {
        const APPS: [(&str, &str); 4] = [
            ("com.tinyspeck.slackmacgap", "Slack"),
            ("com.apple.mobilemail", "Mail"),
            ("com.apple.iCal", "Calendar"),
            ("com.apple.reminders", "Reminders"),
        ];
        const SAMPLES: [(&str, &str, &str, UrgencyLevel); 6] = [
            (
                "緊急対応が必要",
                "プロダクションエラー率が急上昇しています。",
                "監視通知で即時確認が必要なパターン",
                UrgencyLevel::Critical,
            ),
            (
                "15:00会議の招待更新",
                "会議URLが新しいリンクに変更されました。",
                "本日中に確認すべき更新",
                UrgencyLevel::High,
            ),
            (
                "レビュー依頼があります",
                "PR #128 のレビュー依頼が届いています。",
                "作業中断の優先度は中程度",
                UrgencyLevel::Medium,
            ),
            (
                "請求書が発行されました",
                "今月分の請求書を確認してください。",
                "期限前に確認すればよい通知",
                UrgencyLevel::Low,
            ),
            (
                "配達予定が更新されました",
                "荷物の到着予定時刻が変更されました。",
                "状況把握のための一般通知",
                UrgencyLevel::Low,
            ),
            (
                "セキュリティ警告",
                "未確認のログイン試行を検出しました。",
                "アカウント保護のため早め対応",
                UrgencyLevel::High,
            ),
        ];

        let mut next_virtual_id = self
            .collected
            .iter()
            .map(|n| n.id)
            .filter(|id| *id < 0)
            .min()
            .unwrap_or(0);

        for i in 0..count {
            next_virtual_id -= 1;
            let (bundle_id, app_name) = APPS[i % APPS.len()];
            let (summary_line, body, reason, urgency) = SAMPLES[i % SAMPLES.len()];

            self.collected.push(AnalyzedNotification {
                id: next_virtual_id,
                title: summary_line.to_string(),
                body: body.to_string(),
                subtitle: "Dummy".to_string(),
                bundle_id: bundle_id.to_string(),
                app_name: app_name.to_string(),
                urgency,
                summary_line: summary_line.to_string(),
                reason: reason.to_string(),
            });
        }

        count
    }
}

fn app_name_from_bundle(bundle_id: &str) -> String {
    let last = bundle_id.rsplit('.').next().unwrap_or(bundle_id);
    if last.is_empty() {
        bundle_id.to_string()
    } else {
        last.to_string()
    }
}

fn build_analysis_prompt(notification: &Notification) -> String {
    format!(
        "以下の通知を分析してください。\\n\
JSONのみで回答し、追加説明は不要です。\\n\
スキーマ:\\n\
{{\\n\
  \"urgency_level\": \"critical|high|medium|low\",\\n\
  \"summary_line\": \"30文字以内の要約\",\\n\
  \"reason\": \"判定理由を1文\"\\n\
}}\\n\\n\
通知:\\n\
アプリ: {}\\n\
タイトル: {}\\n\
サブタイトル: {}\\n\
本文: {}",
        notification.bundle_id, notification.title, notification.subtitle, notification.body
    )
}

fn parse_analysis_response(
    text: &str,
    notification: &Notification,
) -> Option<NotificationAnalysis> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end < start {
        return None;
    }

    let parsed: Value = serde_json::from_str(&text[start..=end]).ok()?;
    let urgency = match parsed.get("urgency_level").and_then(Value::as_str) {
        Some("critical") => UrgencyLevel::Critical,
        Some("high") => UrgencyLevel::High,
        Some("medium") => UrgencyLevel::Medium,
        Some("low") => UrgencyLevel::Low,
        _ => return None,
    };

    let summary_line = parsed
        .get("summary_line")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| default_summary_line(notification));

    let reason = parsed
        .get("reason")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| "判定理由は取得できませんでした。".to_string());

    Some(NotificationAnalysis {
        urgency,
        summary_line,
        reason,
    })
}

fn fallback_analysis(notification: &Notification) -> NotificationAnalysis {
    NotificationAnalysis {
        urgency: UrgencyLevel::Medium,
        summary_line: default_summary_line(notification),
        reason: "Gemini分析に失敗したため、ローカル規則で中優先として扱いました。".to_string(),
    }
}

fn default_summary_line(notification: &Notification) -> String {
    let text = if !notification.title.trim().is_empty() {
        notification.title.trim().to_string()
    } else if !notification.body.trim().is_empty() {
        notification.body.trim().to_string()
    } else if !notification.subtitle.trim().is_empty() {
        notification.subtitle.trim().to_string()
    } else {
        "内容不明の通知".to_string()
    };

    let mut chars = text.chars().take(60).collect::<String>();
    if text.chars().count() > 60 {
        chars.push('…');
    }
    chars
}

fn build_summary_prompt(notifications: &[AnalyzedNotification]) -> String {
    let body = notifications
        .iter()
        .map(|n| {
            format!(
                "[{}][{}] {}: {}",
                n.app_name,
                n.urgency.label(),
                n.summary_line,
                n.body
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "以下の通知を日本語で簡潔に要約してください。\\n\
アプリごとに整理し、対応順が分かる形にしてください。\\n\\n{}",
        body
    )
}

fn fallback_summary(notifications: &[AnalyzedNotification]) -> String {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut critical = 0;

    for n in notifications {
        *counts.entry(n.app_name.clone()).or_default() += 1;
        if n.urgency == UrgencyLevel::Critical {
            critical += 1;
        }
    }

    let details = counts
        .into_iter()
        .map(|(app, count)| format!("{app}: {count}件"))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "通知 {}件 (R4: {}件)\\n{}",
        notifications.len(),
        critical,
        details
    )
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

fn emit_notifications_updated(app: &AppHandle) {
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

#[tauri::command]
fn get_notification_groups(
    state: State<'_, SharedOrchestrator>,
) -> Result<Vec<UiNotificationGroup>, String> {
    let guard = state
        .0
        .lock()
        .map_err(|err| format!("state lock error: {err}"))?;
    Ok(guard.notification_groups())
}

#[tauri::command]
fn clear_notification(
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
fn clear_app_notifications(
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
fn clear_all_notifications(
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
fn inject_dummy_notifications(
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
fn summarize_notifications(state: State<'_, SharedOrchestrator>) -> Result<String, String> {
    let guard = state
        .0
        .lock()
        .map_err(|err| format!("state lock error: {err}"))?;
    guard
        .summarize_collected()
        .ok_or_else(|| "収集済み通知はありません。".to_string())
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
