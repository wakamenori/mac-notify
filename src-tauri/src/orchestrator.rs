use std::collections::BTreeMap;
use std::env;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use log::{error, warn};

use crate::app_log;
use crate::db::{get_notification_db_path, NotificationDb};
use crate::focus::{get_focus_assertions_path, FocusModeDetector};
use crate::llm::{
    build_analysis_prompt, fallback_analysis_with_reason, parse_analysis_response, AppPrompts,
    CodexErrorCategory, IgnoredApps, LlmClient, UserContext,
};
use crate::models::{
    AnalyzedNotification, FocusState, Notification, NotificationAnalysis, UiNotification,
    UiNotificationGroup, UrgencyLevel,
};
use crate::show_notification;

pub const POLL_INTERVAL_SECONDS: u64 = 5;
pub const MAX_DUMMY_INSERT_COUNT: usize = 30;

#[derive(Clone)]
pub struct SharedOrchestrator(pub Arc<Mutex<NotifyOrchestrator>>);

/// Data returned from the fast Phase 1 (DB read) of the polling cycle.
pub struct PollReadResult {
    /// Notifications that need LLM analysis (filtered, with app_context attached).
    pub pending: Vec<(Notification, Option<String>)>,
    /// User context for LLM prompts (shared across all notifications in the batch).
    pub user_context: Option<String>,
    /// Whether focus mode just ended and we should notify the user.
    pub focus_ended: bool,
}

pub struct NotifyOrchestrator {
    reader: NotificationDb,
    focus_detector: FocusModeDetector,
    app_prompts: AppPrompts,
    ignored_apps: IgnoredApps,
    user_context: UserContext,
    last_rowid: i64,
    collected: Vec<AnalyzedNotification>,
    was_focused: bool,
    poll_count: u64,
}

impl NotifyOrchestrator {
    pub fn new() -> Result<Self> {
        let db_path = get_notification_db_path()?;
        let assertions_path = get_focus_assertions_path();
        app_log::info(format!(
            "orchestrator initializing notification_db={} focus_assertions={}",
            db_path.display(),
            assertions_path.display()
        ));
        let mut reader = NotificationDb::new(db_path);
        let initial_rowid = match reader.latest_rowid() {
            Ok(rowid) => rowid,
            Err(err) => {
                app_log::error(format!("notification DB initial read failed error={err:#}"));
                return Err(err);
            }
        };
        app_log::info(format!(
            "orchestrator initialized initial_rowid={initial_rowid}"
        ));

        let config_dir = env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join(".config/notify");
        let app_prompts = AppPrompts::load(&config_dir.join("app_prompts.json"));
        let ignored_apps = IgnoredApps::load(&config_dir.join("ignored_apps.json"));
        let user_context = UserContext::load(&config_dir.join("user_context.txt"));

        Ok(Self {
            reader,
            focus_detector: FocusModeDetector::new(assertions_path),
            app_prompts,
            ignored_apps,
            user_context,
            last_rowid: initial_rowid,
            collected: Vec::new(),
            was_focused: false,
            poll_count: 0,
        })
    }

    /// Phase 1: Read new notifications from DB and determine focus state.
    /// This is fast (milliseconds) and safe to call while holding the Mutex.
    pub fn poll_read_new(&mut self) -> PollReadResult {
        self.poll_count = self.poll_count.saturating_add(1);
        let is_focused = self.focus_detector.get_state() == FocusState::Active;
        let previous_focus = self.was_focused;
        let mut pending = Vec::new();
        let mut read_count = 0usize;
        let mut ignored_count = 0usize;

        match self.reader.read_new(self.last_rowid) {
            Ok(new_notifications) => {
                read_count = new_notifications.len();
                if let Some(last) = new_notifications.last() {
                    self.last_rowid = last.rowid;
                }
                if is_focused {
                    for notification in new_notifications {
                        if self.ignored_apps.contains(&notification.bundle_id) {
                            ignored_count += 1;
                            continue;
                        }
                        let app_context = self
                            .app_prompts
                            .get(&notification.bundle_id)
                            .map(|s| s.to_string());
                        pending.push((notification, app_context));
                    }
                } else if read_count > 0 {
                    app_log::info(format!(
                        "notifications read but skipped because focus is inactive count={} last_rowid={}",
                        read_count, self.last_rowid
                    ));
                }
            }
            Err(err) => {
                error!("Error reading notification DB: {err:#}");
                app_log::error(format!("notification DB poll read failed error={err:#}"));
            }
        }

        if is_focused != previous_focus {
            app_log::info(format!(
                "focus state changed active={} collected_count={}",
                is_focused,
                self.collected.len()
            ));
        }
        if read_count > 0 || !pending.is_empty() || ignored_count > 0 {
            app_log::info(format!(
                "poll notifications read_count={} pending_count={} ignored_count={} focus_active={} last_rowid={}",
                read_count,
                pending.len(),
                ignored_count,
                is_focused,
                self.last_rowid
            ));
        } else if self.poll_count == 1 || self.poll_count.is_multiple_of(12) {
            app_log::info(format!(
                "poll heartbeat focus_active={} last_rowid={} collected_count={}",
                is_focused,
                self.last_rowid,
                self.collected.len()
            ));
        }

        let focus_ended = !is_focused && self.was_focused && !self.collected.is_empty();
        if focus_ended {
            app_log::info(format!(
                "focus ended collected_count={}",
                self.collected.len()
            ));
        }
        self.was_focused = is_focused;

        let user_ctx = self.user_context.get();
        let user_context = if user_ctx.is_empty() {
            None
        } else {
            Some(user_ctx)
        };

        PollReadResult {
            pending,
            user_context,
            focus_ended,
        }
    }

    /// Phase 3: Store analyzed results back into the orchestrator.
    /// This is fast (milliseconds) and safe to call while holding the Mutex.
    /// Returns true if collected notifications changed.
    pub fn poll_store_results(&mut self, results: Vec<AnalyzedNotification>) -> bool {
        if results.is_empty() {
            return false;
        }
        app_log::info(format!(
            "analysis results stored count={} previous_collected_count={}",
            results.len(),
            self.collected.len()
        ));
        self.collected.extend(results);
        true
    }

    pub fn on_focus_ended(&mut self) {
        let count = self.collected.len();
        app_log::info(format!("focus ended notification displayed count={count}"));
        show_notification("集中モード終了", &format!("{count}件の通知があります"));
    }

    pub fn notification_groups(&self) -> Vec<UiNotificationGroup> {
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
                timestamp: item.timestamp,
            });
        }

        let mut groups: Vec<UiNotificationGroup> = grouped
            .into_iter()
            .map(|(bundle_id, mut notifications)| {
                // Sort notifications newest first
                notifications.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
                let app_name = notifications
                    .first()
                    .map(|n| n.app_name.clone())
                    .unwrap_or_else(|| app_name_from_bundle(&bundle_id));
                let icon_base64 = app_icon_base64(&bundle_id);
                UiNotificationGroup {
                    bundle_id,
                    app_name,
                    icon_base64,
                    notifications,
                }
            })
            .collect();

        // Sort groups by newest notification first
        groups.sort_by(|a, b| {
            let ts_a = a.notifications.first().map(|n| n.timestamp).unwrap_or(0);
            let ts_b = b.notifications.first().map(|n| n.timestamp).unwrap_or(0);
            ts_b.cmp(&ts_a)
        });

        groups
    }

    pub fn urgency_counts(&self) -> [usize; 4] {
        let mut counts = [0usize; 4];
        for n in &self.collected {
            match n.urgency {
                UrgencyLevel::Critical => counts[0] += 1,
                UrgencyLevel::High => counts[1] += 1,
                UrgencyLevel::Medium => counts[2] += 1,
                UrgencyLevel::Low => counts[3] += 1,
            }
        }
        counts
    }

    pub fn clear_notification(&mut self, id: i64) -> bool {
        let before = self.collected.len();
        self.collected.retain(|n| n.id != id);
        self.collected.len() != before
    }

    pub fn clear_app_notifications(&mut self, bundle_id: &str) -> usize {
        let before = self.collected.len();
        self.collected.retain(|n| n.bundle_id != bundle_id);
        before.saturating_sub(self.collected.len())
    }

    pub fn clear_all(&mut self) -> usize {
        let count = self.collected.len();
        self.collected.clear();
        count
    }

    pub fn list_app_prompts(&self) -> Vec<(String, String)> {
        self.app_prompts.list()
    }

    pub fn set_app_prompt(&mut self, bundle_id: String, context: String) -> Result<()> {
        self.app_prompts.set(bundle_id, context);
        self.app_prompts.save()
    }

    pub fn list_ignored_apps(&self) -> Vec<String> {
        self.ignored_apps.list()
    }

    pub fn add_ignored_app(&mut self, bundle_id: String) -> Result<()> {
        self.ignored_apps.add(bundle_id);
        self.ignored_apps.save()
    }

    pub fn remove_ignored_app(&mut self, bundle_id: &str) -> Result<bool> {
        let removed = self.ignored_apps.remove(bundle_id);
        if removed {
            self.ignored_apps.save()?;
        }
        Ok(removed)
    }

    pub fn get_user_context(&self) -> String {
        self.user_context.get()
    }

    pub fn set_user_context(&self, text: &str) -> Result<()> {
        self.user_context.set(text)
    }

    pub fn delete_app_prompt(&mut self, bundle_id: &str) -> Result<bool> {
        let removed = self.app_prompts.remove(bundle_id);
        if removed {
            self.app_prompts.save()?;
        }
        Ok(removed)
    }

    pub fn inject_dummy_notifications(&mut self, count: usize) -> usize {
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

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        // Offsets in seconds to simulate various elapsed times
        const OFFSETS: [i64; 8] = [30, 180, 600, 1800, 3600, 7200, 43200, 86400];

        for i in 0..count {
            next_virtual_id -= 1;
            let (bundle_id, app_name) = APPS[i % APPS.len()];
            let (summary_line, body, reason, urgency) = SAMPLES[i % SAMPLES.len()];
            let offset = OFFSETS[i % OFFSETS.len()];

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
                timestamp: now - offset,
            });
        }

        count
    }
}

/// Phase 2: Analyze notifications using the LLM. Runs outside the Mutex.
/// Returns analyzed notifications and a list of critical ones (for dialog display).
pub fn analyze_notifications_batch(
    llm: &LlmClient,
    pending: Vec<(Notification, Option<String>)>,
    user_context: Option<&str>,
) -> (Vec<AnalyzedNotification>, Vec<AnalyzedNotification>) {
    let mut results = Vec::new();
    let mut criticals = Vec::new();

    for (notification, app_context) in pending {
        let analysis = analyze_single(llm, &notification, app_context.as_deref(), user_context);

        let analyzed = AnalyzedNotification {
            id: notification.rowid,
            title: notification.title,
            body: notification.body,
            subtitle: notification.subtitle,
            bundle_id: notification.bundle_id.clone(),
            app_name: app_name_from_bundle(&notification.bundle_id),
            urgency: analysis.urgency,
            summary_line: analysis.summary_line,
            reason: analysis.reason,
            timestamp: notification.timestamp,
        };

        if analysis.urgency == UrgencyLevel::Critical {
            criticals.push(analyzed.clone());
        }
        results.push(analyzed);
    }

    (results, criticals)
}

fn analyze_single(
    llm: &LlmClient,
    notification: &Notification,
    app_context: Option<&str>,
    user_context: Option<&str>,
) -> NotificationAnalysis {
    if !llm.can_use() {
        warn!("Codex CLI is not available");
        app_log::warn(format!(
            "analysis skipped codex_unavailable rowid={} bundle_id={}",
            notification.rowid, notification.bundle_id
        ));
        return NotificationAnalysis {
            urgency: UrgencyLevel::Medium,
            summary_line: crate::llm::default_summary_line(notification),
            reason: "Codex CLIを利用できないため分析できませんでした。Codex CLIまたはCodexアプリがインストールされ、必要に応じて`codex login`済みか確認してください。"
                .to_string(),
        };
    }

    let prompt = build_analysis_prompt(notification, app_context, user_context);
    app_log::info(format!(
        "analysis started rowid={} bundle_id={} prompt_chars={} app_context={}",
        notification.rowid,
        notification.bundle_id,
        prompt.chars().count(),
        app_context.is_some()
    ));
    match llm.generate_text(&prompt) {
        Ok(text) => match parse_analysis_response(&text, notification) {
            Some(parsed) => {
                app_log::info(format!(
                    "analysis parsed rowid={} bundle_id={} urgency={:?}",
                    notification.rowid, notification.bundle_id, parsed.urgency
                ));
                parsed
            }
            None => {
                warn!(
                    "analysis response parse failed for {} (response length: {})",
                    notification.rowid,
                    text.len()
                );
                app_log::warn(format!(
                    "analysis parse failed rowid={} bundle_id={} response_chars={}",
                    notification.rowid,
                    notification.bundle_id,
                    text.chars().count()
                ));
                fallback_analysis_with_reason(
                    notification,
                    "Codex CLIの応答を期待するJSON形式として解析できなかったため、中優先として扱いました。"
                        .to_string(),
                )
            }
        },
        Err(err) => {
            warn!("notification analysis failed: {err:#}");
            app_log::warn(format!(
                "analysis failed rowid={} bundle_id={} error={}",
                notification.rowid,
                notification.bundle_id,
                codex_error_summary(&err.to_string())
            ));
            let detail = err.to_string().to_lowercase();
            if detail.contains("timed out") || detail.contains("timeout") {
                fallback_analysis_with_reason(
                    notification,
                    format!(
                        "Codex CLI `{}` の応答がタイムアウトしたため、中優先として扱いました。",
                        llm.current_model()
                    ),
                )
            } else {
                fallback_analysis_with_reason(
                    notification,
                    format!(
                        "{} 中優先として扱いました。",
                        codex_error_summary(&err.to_string())
                    ),
                )
            }
        }
    }
}

fn codex_error_summary(error: &str) -> String {
    if error.contains("Codex CLIの実行に失敗しました") {
        return error.to_string();
    }

    let category = CodexErrorCategory::classify(error);
    match category {
        CodexErrorCategory::Unknown => {
            format!("Codex CLIの実行に失敗しました（詳細: {error}）。")
        }
        _ => category.description().to_string(),
    }
}

pub fn app_name_from_bundle(bundle_id: &str) -> String {
    use std::collections::HashMap;
    use std::sync::Mutex;

    static CACHE: std::sync::LazyLock<Mutex<HashMap<String, String>>> =
        std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

    if let Ok(cache) = CACHE.lock() {
        if let Some(name) = cache.get(bundle_id) {
            return name.clone();
        }
    }

    let name = resolve_app_display_name(bundle_id);

    if let Ok(mut cache) = CACHE.lock() {
        cache.insert(bundle_id.to_string(), name.clone());
    }

    name
}

pub fn app_icon_base64(bundle_id: &str) -> Option<String> {
    use std::collections::HashMap;
    use std::sync::Mutex;

    static CACHE: std::sync::LazyLock<Mutex<HashMap<String, Option<String>>>> =
        std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

    if let Ok(cache) = CACHE.lock() {
        if let Some(icon) = cache.get(bundle_id) {
            return icon.clone();
        }
    }

    let icon = resolve_app_icon(bundle_id);

    if let Ok(mut cache) = CACHE.lock() {
        cache.insert(bundle_id.to_string(), icon.clone());
    }

    icon
}

fn resolve_app_icon(bundle_id: &str) -> Option<String> {
    // Use swift + NSWorkspace to get the app icon as base64 PNG (works for all apps including Asset Catalog icons)
    let script = r#"
import AppKit
let bid = CommandLine.arguments[1]
guard let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: bid) else { exit(1) }
let icon = NSWorkspace.shared.icon(forFile: url.path)
let size = NSSize(width: 32, height: 32)
let img = NSImage(size: size)
img.lockFocus()
icon.draw(in: NSRect(origin: .zero, size: size))
img.unlockFocus()
guard let tiff = img.tiffRepresentation,
      let rep = NSBitmapImageRep(data: tiff),
      let png = rep.representation(using: .png, properties: [:]) else { exit(1) }
print(png.base64EncodedString())
"#;

    let output = std::process::Command::new("swift")
        .args(["-e", script, bundle_id])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let b64 = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if b64.is_empty() {
        None
    } else {
        Some(b64)
    }
}

fn resolve_app_display_name(bundle_id: &str) -> String {
    // Use mdfind to locate the .app bundle, then read display name from Info.plist
    if let Ok(output) = std::process::Command::new("mdfind")
        .arg(format!("kMDItemCFBundleIdentifier == '{bundle_id}'"))
        .output()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Some(app_path) = stdout.lines().next().filter(|l| !l.is_empty()) {
            let plist_path = format!("{app_path}/Contents/Info.plist");
            // Try CFBundleDisplayName first, then CFBundleName
            for key in ["CFBundleDisplayName", "CFBundleName"] {
                if let Ok(out) = std::process::Command::new("/usr/libexec/PlistBuddy")
                    .args(["-c", &format!("Print :{key}"), &plist_path])
                    .output()
                {
                    if out.status.success() {
                        let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
                        if !name.is_empty() {
                            return name;
                        }
                    }
                }
            }
        }
    }

    // Fallback: use last segment of bundle_id
    let last = bundle_id.rsplit('.').next().unwrap_or(bundle_id);
    if last.is_empty() {
        bundle_id.to_string()
    } else {
        last.to_string()
    }
}
