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
                } else if let Ok(flat) =
                    serde_json::from_str::<HashMap<String, String>>(&content)
                {
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
            .map(|(k, v)| {
                (
                    k.as_str(),
                    serde_json::json!({ "context": v.context }),
                )
            })
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

const GEMINI_MODEL: &str = "gemini-2.5-flash-lite";

pub struct GeminiClient {
    api_key: String,
    client: Client,
}

impl GeminiClient {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("failed to build reqwest client"),
        }
    }

    pub fn can_use(&self) -> bool {
        !self.api_key.is_empty()
    }

    pub fn generate_text(&self, prompt: &str) -> Result<String> {
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

pub fn build_analysis_prompt(notification: &Notification, app_context: Option<&str>) -> String {
    let mut prompt = format!(
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
        reason: "Gemini分析に失敗したため、ローカル規則で中優先として扱いました。".to_string(),
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

