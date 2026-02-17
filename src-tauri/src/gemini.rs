use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{bail, Result};
use reqwest::blocking::Client;
use serde_json::{json, Value};

use crate::models::{AnalyzedNotification, Notification, NotificationAnalysis, UrgencyLevel};

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

pub fn build_analysis_prompt(notification: &Notification) -> String {
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

pub fn build_summary_prompt(notifications: &[AnalyzedNotification]) -> String {
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

pub fn fallback_summary(notifications: &[AnalyzedNotification]) -> String {
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
