use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use log::warn;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::models::{Notification, NotificationAnalysis, UrgencyLevel};

const SLACK_BUNDLE_ID: &str = "com.tinyspeck.slackmacgap";
const SLACK_NEW_MESSAGE_SUFFIX: &str = " の新しいメッセージ";
const SLACK_INTEGRATION_SUFFIX: &str = " からの新しいメッセージ";

#[derive(Debug, Deserialize)]
pub struct AppPromptConfig {
    pub context: String,
}

#[derive(Debug)]
pub struct AppPrompts {
    map: HashMap<String, AppPromptConfig>,
    path: PathBuf,
}

impl Default for AppPrompts {
    fn default() -> Self {
        Self {
            map: HashMap::new(),
            path: PathBuf::new(),
        }
    }
}

impl AppPrompts {
    pub fn load(path: &Path) -> Self {
        let map = match fs::read_to_string(path) {
            Ok(content) => {
                // Try nested format first: {"bundleId": {"context": "..."}}
                if let Ok(parsed) =
                    serde_json::from_str::<HashMap<String, AppPromptConfig>>(&content)
                {
                    parsed
                // Fall back to flat format: {"bundleId": "context string"}
                } else if let Ok(flat) = serde_json::from_str::<HashMap<String, String>>(&content) {
                    flat.into_iter()
                        .map(|(k, v)| (k, AppPromptConfig { context: v }))
                        .collect()
                } else {
                    warn!("Failed to parse app_prompts.json");
                    HashMap::new()
                }
            }
            Err(_) => HashMap::new(),
        };
        Self {
            map,
            path: path.to_path_buf(),
        }
    }

    pub fn get(&self, bundle_id: &str) -> Option<&str> {
        self.map.get(bundle_id).map(|c| c.context.as_str())
    }

    pub fn list(&self) -> Vec<(String, String)> {
        self.map
            .iter()
            .map(|(k, v)| (k.clone(), v.context.clone()))
            .collect()
    }

    pub fn set(&mut self, bundle_id: String, context: String) {
        self.map.insert(bundle_id, AppPromptConfig { context });
    }

    pub fn remove(&mut self, bundle_id: &str) -> bool {
        self.map.remove(bundle_id).is_some()
    }

    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let serializable: BTreeMap<&str, serde_json::Value> = self
            .map
            .iter()
            .map(|(k, v)| (k.as_str(), serde_json::json!({ "context": v.context })))
            .collect();
        let json = serde_json::to_string_pretty(&serializable)?;
        fs::write(&self.path, json)?;
        Ok(())
    }
}

#[derive(Debug)]
pub struct IgnoredApps {
    set: HashSet<String>,
    path: PathBuf,
}

impl Default for IgnoredApps {
    fn default() -> Self {
        Self {
            set: HashSet::new(),
            path: PathBuf::new(),
        }
    }
}

impl IgnoredApps {
    pub fn load(path: &Path) -> Self {
        let set = match fs::read_to_string(path) {
            Ok(content) => match serde_json::from_str::<Vec<String>>(&content) {
                Ok(parsed) => parsed.into_iter().collect(),
                Err(err) => {
                    warn!("Failed to parse ignored_apps.json: {err:#}");
                    HashSet::new()
                }
            },
            Err(_) => HashSet::new(),
        };
        Self {
            set,
            path: path.to_path_buf(),
        }
    }

    pub fn contains(&self, bundle_id: &str) -> bool {
        self.set.contains(bundle_id)
    }

    pub fn list(&self) -> Vec<String> {
        let mut v: Vec<String> = self.set.iter().cloned().collect();
        v.sort();
        v
    }

    pub fn add(&mut self, bundle_id: String) {
        self.set.insert(bundle_id);
    }

    pub fn remove(&mut self, bundle_id: &str) -> bool {
        self.set.remove(bundle_id)
    }

    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let sorted = self.list();
        let json = serde_json::to_string_pretty(&sorted)?;
        fs::write(&self.path, json)?;
        Ok(())
    }
}

const LLM_MODEL: &str = "qwen3.5:latest";
const LLM_REQUEST_TIMEOUT_SECONDS: u64 = 180;
const OLLAMA_CONNECT_TIMEOUT_SECONDS: u64 = 2;
const LLM_MAX_OUTPUT_TOKENS: u64 = 160;
pub const OLLAMA_BASE_URL: &str = "http://localhost:11434";

#[derive(Debug, Deserialize, Serialize)]
struct LlmSettings {
    model: String,
}

impl Default for LlmSettings {
    fn default() -> Self {
        Self {
            model: LLM_MODEL.to_string(),
        }
    }
}

impl LlmSettings {
    fn load(path: &Path) -> Self {
        match fs::read_to_string(path) {
            Ok(content) => match serde_json::from_str::<LlmSettings>(&content) {
                Ok(parsed) => parsed,
                Err(err) => {
                    warn!("Failed to parse llm_settings.json: {err:#}");
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }

    fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        fs::write(path, json)?;
        Ok(())
    }
}

fn notify_config_dir() -> PathBuf {
    env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".config/notify")
}

#[derive(Clone)]
pub struct SharedLlm(pub Arc<LlmClient>);

pub struct LlmClient {
    client: Client,
    model: Mutex<String>,
    settings_path: PathBuf,
}

impl LlmClient {
    pub fn new() -> Self {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(OLLAMA_CONNECT_TIMEOUT_SECONDS))
            .timeout(Duration::from_secs(LLM_REQUEST_TIMEOUT_SECONDS))
            .build()
            .expect("failed to build reqwest client");

        let settings_path = notify_config_dir().join("llm_settings.json");
        let settings = LlmSettings::load(&settings_path);

        Self {
            client,
            model: Mutex::new(settings.model),
            settings_path,
        }
    }

    pub fn can_use(&self) -> bool {
        self.client.get(OLLAMA_BASE_URL).send().is_ok()
    }

    pub fn current_model(&self) -> String {
        self.model
            .lock()
            .map(|model| model.clone())
            .unwrap_or_else(|_| LLM_MODEL.to_string())
    }

    pub fn list_models(&self) -> Result<Vec<String>> {
        let output = Command::new("ollama")
            .arg("list")
            .output()
            .context("failed to execute `ollama list`")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("`ollama list` failed: {}", stderr.trim());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let models = stdout
            .lines()
            .skip(1)
            .filter_map(|line| line.split_whitespace().next())
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(ToString::to_string)
            .collect();

        Ok(models)
    }

    pub fn set_model(&self, model: String) -> Result<()> {
        let model = model.trim();
        if model.is_empty() {
            bail!("Model name is required")
        }

        let available_models = self.list_models()?;
        if !available_models.iter().any(|candidate| candidate == model) {
            bail!("Model `{model}` is not installed in Ollama")
        }

        let settings = LlmSettings {
            model: model.to_string(),
        };
        settings.save(&self.settings_path)?;

        let mut current = self
            .model
            .lock()
            .map_err(|err| anyhow::anyhow!("model state lock error: {err}"))?;
        *current = model.to_string();
        Ok(())
    }

    pub fn generate_text(&self, prompt: &str) -> Result<String> {
        if !self.can_use() {
            bail!("Ollama is not running at {OLLAMA_BASE_URL}")
        }

        let endpoint = format!("{OLLAMA_BASE_URL}/api/generate");
        let model = self.current_model();

        let response: Value = self
            .client
            .post(endpoint)
            .json(&json!({
                "model": model,
                "prompt": prompt,
                "stream": false,
                "format": "json",
                "think": false,
                "options": {
                    "num_predict": LLM_MAX_OUTPUT_TOKENS,
                    "temperature": 0
                }
            }))
            .send()
            .with_context(|| format!("request to Ollama model `{model}` failed"))?
            .error_for_status()
            .with_context(|| format!("Ollama model `{model}` returned an error status"))?
            .json()
            .with_context(|| format!("failed to parse Ollama response for model `{model}`"))?;

        let text = response
            .get("response")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();

        if text.is_empty() {
            bail!("LLM response text is empty")
        }

        // Remove Qwen3 thinking blocks
        let text = strip_thinking_tags(&text);

        Ok(text)
    }
}

fn strip_thinking_tags(text: &str) -> String {
    use regex::Regex;
    let re = Regex::new(r"<think>[\s\S]*?</think>").expect("invalid regex");
    re.replace_all(text, "").trim().to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PromptNotificationKind {
    Raw,
    SlackChannelMessage,
    SlackIntegrationNotification,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PromptNotificationView {
    kind: PromptNotificationKind,
    title: String,
    subtitle: String,
    body: String,
    detail_lines: Vec<String>,
}

fn build_prompt_notification_view(notification: &Notification) -> PromptNotificationView {
    if notification.bundle_id == SLACK_BUNDLE_ID {
        if let Some(view) = build_slack_prompt_view(notification) {
            return view;
        }
    }

    PromptNotificationView {
        kind: PromptNotificationKind::Raw,
        title: notification.title.clone(),
        subtitle: notification.subtitle.clone(),
        body: notification.body.clone(),
        detail_lines: Vec::new(),
    }
}

fn build_slack_prompt_view(notification: &Notification) -> Option<PromptNotificationView> {
    let title = notification.title.trim();
    let body = notification.body.trim();
    let subtitle = notification.subtitle.trim();

    if let Some(conversation_name) = title.strip_suffix(SLACK_NEW_MESSAGE_SUFFIX) {
        if let Some((sender, message_text)) = body.split_once(": ") {
            return Some(PromptNotificationView {
                kind: PromptNotificationKind::SlackChannelMessage,
                title: title.to_string(),
                subtitle: subtitle.to_string(),
                body: body.to_string(),
                detail_lines: vec![
                    "通知種別: slack_channel_message".to_string(),
                    format!("会話名: {}", conversation_name.trim()),
                    format!("送信者表示名: {}", sender.trim()),
                    format!("メッセージ本文: {}", message_text.trim()),
                ],
            });
        }
    }

    if let Some(source_name) = title.strip_suffix(SLACK_INTEGRATION_SUFFIX) {
        return Some(PromptNotificationView {
            kind: PromptNotificationKind::SlackIntegrationNotification,
            title: title.to_string(),
            subtitle: subtitle.to_string(),
            body: body.to_string(),
            detail_lines: vec![
                "通知種別: slack_integration_notification".to_string(),
                format!("通知元表示名: {}", source_name.trim()),
                format!("通知本文: {body}"),
            ],
        });
    }

    None
}

pub fn build_analysis_prompt(notification: &Notification, app_context: Option<&str>) -> String {
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S (%a)");
    let prompt_view = build_prompt_notification_view(notification);
    let mut prompt = format!(
        "現在日時: {now}\\n\\n\
以下の通知を分析してください。\\n\
JSONのみで回答し、追加説明は不要です。\\n\\n\
緊急度の判定基準（遅延コストで判断）:\\n\
- critical: 今すぐ対応しないと実害が出る。分単位で損害が拡大する（例: 本番障害、セキュリティインシデント、家族からの緊急連絡）\\n\
- high: 集中終了後すぐ見るべき。数時間放置すると困る（例: 上司からの直接メンション、今日締切のリマインダー、承認待ちのブロッカー）\\n\
- medium: 後で確認すれば十分。半日〜1日遅れても問題ない（例: PRレビュー依頼、一般的なチャット、ミーティング通知）\\n\
- low: 見なくてもほぼ困らない。無視しても実害なし（例: マーケティング通知、SNSのいいね、アプリ更新案内）\\n\\n\
スキーマ:\\n\
{{\\n\
  \"summary_line\": \"誰から何の用件か一目で分かる要約\",\\n\
  \"reason\": \"判定理由を1文\",\\n\
  \"urgency_level\": \"critical|high|medium|low\"\\n\
}}\\n\\n\
summary_lineの例:\\n\
- 良い例: \"田中さんがPR #42にレビューコメント\"\\n\
- 良い例: \"本番DBのCPU使用率が95%超過\"\\n\
- 悪い例: \"PRにコメントあり\"\\n\
- 悪い例: \"アラート発生\"\\n\\n\
通知:\\n\
アプリ: {}\\n\
タイトル: {}\\n\
サブタイトル: {}\\n\
本文: {}",
        notification.bundle_id, prompt_view.title, prompt_view.subtitle, prompt_view.body
    );

    if !prompt_view.detail_lines.is_empty() {
        prompt.push_str("\\n");
        for line in &prompt_view.detail_lines {
            prompt.push_str("\\n");
            prompt.push_str(line);
        }
    }

    if let Some(ctx) = app_context {
        prompt.push_str(&format!("\\n\\nこのアプリに関する追加コンテキスト: {ctx}"));
    }

    prompt
}

pub fn parse_analysis_response(
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
        .map(|s| truncate_chars(s, 60))
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

pub fn fallback_analysis(notification: &Notification) -> NotificationAnalysis {
    fallback_analysis_with_reason(
        notification,
        "LLM分析に失敗したため、ローカル規則で中優先として扱いました。".to_string(),
    )
}

pub fn fallback_analysis_with_reason(
    notification: &Notification,
    reason: String,
) -> NotificationAnalysis {
    NotificationAnalysis {
        urgency: UrgencyLevel::Medium,
        summary_line: default_summary_line(notification),
        reason,
    }
}

pub fn default_summary_line(notification: &Notification) -> String {
    let text = if !notification.title.trim().is_empty() {
        notification.title.trim().to_string()
    } else if !notification.body.trim().is_empty() {
        notification.body.trim().to_string()
    } else if !notification.subtitle.trim().is_empty() {
        notification.subtitle.trim().to_string()
    } else {
        "内容不明の通知".to_string()
    };

    truncate_chars(&text, 60)
}

fn truncate_chars(s: &str, max: usize) -> String {
    let mut chars = s.chars().take(max).collect::<String>();
    if s.chars().count() > max {
        chars.push('…');
    }
    chars
}

#[cfg(test)]
mod tests {
    use super::{
        build_analysis_prompt, build_prompt_notification_view, PromptNotificationKind,
        SLACK_BUNDLE_ID,
    };
    use crate::models::Notification;

    fn sample_notification(title: &str, body: &str) -> Notification {
        Notification {
            rowid: 1,
            title: title.to_string(),
            body: body.to_string(),
            subtitle: String::new(),
            bundle_id: SLACK_BUNDLE_ID.to_string(),
            timestamp: 0,
        }
    }

    #[test]
    fn slack_channel_message_is_preprocessed_for_prompt() {
        let notification = sample_notification(
            "#ns_zatsu の新しいメッセージ",
            "Jo Okazaki（ジョー）: ほしくなる",
        );

        let view = build_prompt_notification_view(&notification);

        assert_eq!(view.kind, PromptNotificationKind::SlackChannelMessage);
        assert!(view
            .detail_lines
            .contains(&"会話名: #ns_zatsu".to_string()));
        assert!(view
            .detail_lines
            .contains(&"送信者表示名: Jo Okazaki（ジョー）".to_string()));
        assert!(view.detail_lines.contains(&"メッセージ本文: ほしくなる".to_string()));
    }

    #[test]
    fn slack_integration_message_is_preprocessed_for_prompt() {
        let notification = sample_notification(
            "バクラク勤怠 からの新しいメッセージ",
            "勤怠エラーが4日分あります",
        );

        let view = build_prompt_notification_view(&notification);

        assert_eq!(view.kind, PromptNotificationKind::SlackIntegrationNotification);
        assert!(view
            .detail_lines
            .contains(&"通知元表示名: バクラク勤怠".to_string()));
    }

    #[test]
    fn prompt_keeps_original_fields_and_adds_preprocessed_slack_details() {
        let notification = sample_notification(
            "#ns_zatsu の新しいメッセージ",
            "Jo Okazaki（ジョー）: ほしくなる",
        );

        let prompt = build_analysis_prompt(&notification, Some("Slackワークスペースの社内連絡"));

        assert!(prompt.contains("タイトル: #ns_zatsu の新しいメッセージ"));
        assert!(prompt.contains("本文: Jo Okazaki（ジョー）: ほしくなる"));
        assert!(prompt.contains("通知種別: slack_channel_message"));
        assert!(prompt.contains("送信者表示名: Jo Okazaki（ジョー）"));
        assert!(prompt.contains("メッセージ本文: ほしくなる"));
        assert!(prompt.contains("このアプリに関する追加コンテキスト: Slackワークスペースの社内連絡"));
    }
}
