use std::collections::BTreeMap;
use std::env;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use log::{error, warn};

use crate::db::{get_notification_db_path, NotificationDb};
use crate::focus::{get_focus_assertions_path, FocusModeDetector};
use crate::llm::{
    build_analysis_prompt, fallback_analysis, parse_analysis_response, AppPrompts, IgnoredApps,
    LlmClient, OLLAMA_BASE_URL,
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
    /// Whether focus mode just ended and we should notify the user.
    pub focus_ended: bool,
}

pub struct NotifyOrchestrator {
    reader: NotificationDb,
    focus_detector: FocusModeDetector,
    app_prompts: AppPrompts,
    ignored_apps: IgnoredApps,
    last_rowid: i64,
    collected: Vec<AnalyzedNotification>,
    was_focused: bool,
}

impl NotifyOrchestrator {
    pub fn new() -> Result<Self> {
        let db_path = get_notification_db_path()?;
        let assertions_path = get_focus_assertions_path();
        let mut reader = NotificationDb::new(db_path);
        let initial_rowid = reader.latest_rowid()?;

        let config_dir = env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join(".config/mac-notify");
        let app_prompts = AppPrompts::load(&config_dir.join("app_prompts.json"));
        let ignored_apps = IgnoredApps::load(&config_dir.join("ignored_apps.json"));

        Ok(Self {
            reader,
            focus_detector: FocusModeDetector::new(assertions_path),
            app_prompts,
            ignored_apps,
            last_rowid: initial_rowid,
            collected: Vec::new(),
            was_focused: false,
        })
    }

    /// Phase 1: Read new notifications from DB and determine focus state.
    /// This is fast (milliseconds) and safe to call while holding the Mutex.
    pub fn poll_read_new(&mut self) -> PollReadResult {
        let is_focused = self.focus_detector.get_state() == FocusState::Active;
        let mut pending = Vec::new();

        match self.reader.read_new(self.last_rowid) {
            Ok(new_notifications) => {
                if let Some(last) = new_notifications.last() {
                    self.last_rowid = last.rowid;
                }
                if is_focused {
                    for notification in new_notifications {
                        if self.ignored_apps.contains(&notification.bundle_id) {
                            continue;
                        }
                        let app_context = self
                            .app_prompts
                            .get(&notification.bundle_id)
                            .map(|s| s.to_string());
                        pending.push((notification, app_context));
                    }
                }
            }
            Err(err) => {
                error!("Error reading notification DB: {err:#}");
            }
        }

        let focus_ended = !is_focused && self.was_focused && !self.collected.is_empty();
        self.was_focused = is_focused;

        PollReadResult {
            pending,
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
        self.collected.extend(results);
        true
    }

    pub fn on_focus_ended(&mut self) {
        let count = self.collected.len();
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
                UiNotificationGroup {
                    bundle_id,
                    app_name,
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
) -> (Vec<AnalyzedNotification>, Vec<AnalyzedNotification>) {
    let mut results = Vec::new();
    let mut criticals = Vec::new();

    for (notification, app_context) in pending {
        let analysis = analyze_single(llm, &notification, app_context.as_deref());

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
) -> NotificationAnalysis {
    if !llm.can_use() {
        warn!("Ollama is not running at {OLLAMA_BASE_URL}");
        return NotificationAnalysis {
            urgency: UrgencyLevel::Medium,
            summary_line: crate::llm::default_summary_line(notification),
            reason: "Ollamaが起動していないため分析できませんでした。`ollama serve` を実行してください。"
                .to_string(),
        };
    }

    let prompt = build_analysis_prompt(notification, app_context);
    match llm.generate_text(&prompt) {
        Ok(text) => match parse_analysis_response(&text, notification) {
            Some(parsed) => return parsed,
            None => warn!("analysis response parse failed for {}", notification.rowid),
        },
        Err(err) => warn!("notification analysis failed: {err:#}"),
    }

    fallback_analysis(notification)
}

pub fn app_name_from_bundle(bundle_id: &str) -> String {
    let last = bundle_id.rsplit('.').next().unwrap_or(bundle_id);
    if last.is_empty() {
        bundle_id.to_string()
    } else {
        last.to_string()
    }
}
