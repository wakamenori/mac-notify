use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Result};
use log::warn;
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::models::{Notification, NotificationAnalysis, UrgencyLevel};

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

const LLM_MODEL: &str = "qwen3:8b";
pub const OLLAMA_BASE_URL: &str = "http://localhost:11434";

pub struct LlmClient {
    client: Client,
    available: bool,
}

impl LlmClient {
    pub fn new() -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .expect("failed to build reqwest client");

        let available = client.get(OLLAMA_BASE_URL).send().is_ok();

        Self { client, available }
    }

    pub fn can_use(&self) -> bool {
        self.available
    }

    pub fn generate_text(&self, prompt: &str) -> Result<String> {
        if !self.can_use() {
            bail!("Ollama is not running at {OLLAMA_BASE_URL}")
        }

        let endpoint = format!("{OLLAMA_BASE_URL}/api/generate");

        let response: Value = self
            .client
            .post(endpoint)
            .json(&json!({
                "model": LLM_MODEL,
                "prompt": prompt,
                "stream": false
            }))
            .send()?
            .error_for_status()?
            .json()?;

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

pub fn build_analysis_prompt(notification: &Notification, app_context: Option<&str>) -> String {
    let mut prompt = format!(
        "以下の通知を分析してください。\\n\
JSONのみで回答し、追加説明は不要です。\\n\\n\
緊急度の判定基準（遅延コストで判断）:\\n\
- critical: 今すぐ対応しないと実害が出る。分単位で損害が拡大する（例: 本番障害、セキュリティインシデント、家族からの緊急連絡）\\n\
- high: 集中終了後すぐ見るべき。数時間放置すると困る（例: 上司からの直接メンション、今日締切のリマインダー、承認待ちのブロッカー）\\n\
- medium: 後で確認すれば十分。半日〜1日遅れても問題ない（例: PRレビュー依頼、一般的なチャット、ミーティング通知）\\n\
- low: 見なくてもほぼ困らない。無視しても実害なし（例: マーケティング通知、SNSのいいね、アプリ更新案内）\\n\\n\
スキーマ:\\n\
{{\\n\
  \"summary_line\": \"30文字以内の要約\",\\n\
  \"reason\": \"判定理由を1文\",\\n\
  \"urgency_level\": \"critical|high|medium|low\"\\n\
}}\\n\\n\
通知:\\n\
アプリ: {}\\n\
タイトル: {}\\n\
サブタイトル: {}\\n\
本文: {}",
        notification.bundle_id, notification.title, notification.subtitle, notification.body
    );

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

pub fn fallback_analysis(notification: &Notification) -> NotificationAnalysis {
    NotificationAnalysis {
        urgency: UrgencyLevel::Medium,
        summary_line: default_summary_line(notification),
        reason: "LLM分析に失敗したため、ローカル規則で中優先として扱いました。".to_string(),
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

    let mut chars = text.chars().take(60).collect::<String>();
    if text.chars().count() > 60 {
        chars.push('…');
    }
    chars
}
