use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use log::warn;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::app_log;
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

pub struct UserContext {
    path: PathBuf,
}

impl UserContext {
    pub fn load(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
        }
    }

    pub fn get(&self) -> String {
        fs::read_to_string(&self.path)
            .unwrap_or_default()
            .trim()
            .to_string()
    }

    pub fn set(&self, text: &str) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&self.path, text.trim())?;
        Ok(())
    }
}

const CODEX_CLI_LABEL: &str = "codex-cli";
const CODEX_REASONING_EFFORT: &str = "low";
const LLM_REQUEST_TIMEOUT_SECONDS: u64 = 180;
const CODEX_POLL_INTERVAL_MS: u64 = 100;
const CODEX_CLI_FALLBACK_PATHS: [&str; 3] = [
    "/opt/homebrew/bin/codex",
    "/usr/local/bin/codex",
    "/Applications/Codex.app/Contents/Resources/codex",
];
const CODEX_ANALYSIS_OUTPUT_SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "summary_line": {
      "type": "string",
      "description": "誰から何の用件か一目で分かる60文字以内の要約"
    },
    "reason": {
      "type": "string",
      "description": "判定理由を1文で要約。外部コンテキストの本文、ID、個人情報、機密情報はそのまま引用しない"
    },
    "urgency_level": {
      "type": "string",
      "enum": ["critical", "high", "medium", "low"]
    }
  },
  "required": ["summary_line", "reason", "urgency_level"],
  "additionalProperties": false
}"#;

#[derive(Debug, Deserialize, Serialize)]
struct LlmSettings {
    model: String,
}

impl Default for LlmSettings {
    fn default() -> Self {
        Self {
            model: CODEX_CLI_LABEL.to_string(),
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

fn codex_diagnostics_log_path() -> PathBuf {
    if let Ok(path) = env::var("MAC_NOTIFY_CODEX_DIAGNOSTICS_LOG") {
        return PathBuf::from(path);
    }
    notify_config_dir().join("codex_diagnostics.log")
}

#[derive(Clone)]
pub struct SharedLlm(pub Arc<LlmClient>);

pub struct LlmClient {
    model: Mutex<String>,
    settings_path: PathBuf,
}

impl LlmClient {
    pub fn new() -> Self {
        let settings_path = notify_config_dir().join("llm_settings.json");
        let mut settings = LlmSettings::load(&settings_path);
        if settings.model != CODEX_CLI_LABEL {
            settings.model = CODEX_CLI_LABEL.to_string();
            if let Err(err) = settings.save(&settings_path) {
                warn!("failed to persist model reset: {err}");
            }
        }
        app_log::info(format!(
            "llm client initialized backend={} reasoning_effort={} settings_path={}",
            CODEX_CLI_LABEL,
            CODEX_REASONING_EFFORT,
            settings_path.display()
        ));

        Self {
            model: Mutex::new(settings.model),
            settings_path,
        }
    }

    pub fn can_use(&self) -> bool {
        resolve_codex_cli().is_some()
    }

    pub fn current_model(&self) -> String {
        self.model
            .lock()
            .map(|model| model.clone())
            .unwrap_or_else(|_| CODEX_CLI_LABEL.to_string())
    }

    pub fn list_models(&self) -> Result<Vec<String>> {
        if !self.can_use() {
            bail!("Codex CLI is not available. Install Codex CLI or the Codex app.")
        }
        Ok(vec![CODEX_CLI_LABEL.to_string()])
    }

    pub fn set_model(&self, model: String) -> Result<()> {
        let model = model.trim();
        if model != CODEX_CLI_LABEL {
            bail!("Codex CLI backend only supports `{CODEX_CLI_LABEL}`")
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
        let codex_cli = resolve_codex_cli()
            .context("Codex CLI is not available. Install Codex CLI or the Codex app.")?;
        app_log::info(format!(
            "codex exec starting cli={} reasoning_effort={} prompt_chars={} timeout_seconds={}",
            codex_cli.display(),
            CODEX_REASONING_EFFORT,
            prompt.chars().count(),
            LLM_REQUEST_TIMEOUT_SECONDS
        ));

        let output_path = codex_temp_path("output.json");
        let stdout_path = codex_temp_path("stdout.log");
        let stderr_path = codex_temp_path("stderr.log");
        let schema_path = write_codex_schema_file()?;
        let stdout_file = File::create(&stdout_path).with_context(|| {
            format!(
                "failed to create Codex CLI stdout file at {}",
                stdout_path.display()
            )
        })?;
        let stderr_file = File::create(&stderr_path).with_context(|| {
            format!(
                "failed to create Codex CLI stderr file at {}",
                stderr_path.display()
            )
        })?;

        let child_result = Command::new(&codex_cli)
            .arg("-c")
            .arg(format!(
                "model_reasoning_effort=\"{CODEX_REASONING_EFFORT}\""
            ))
            .arg("--ask-for-approval")
            .arg("never")
            .arg("exec")
            .arg("--ephemeral")
            .arg("--ignore-rules")
            .arg("--skip-git-repo-check")
            .arg("--sandbox")
            .arg("read-only")
            .arg("--output-schema")
            .arg(&schema_path)
            .arg("--output-last-message")
            .arg(&output_path)
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::from(stdout_file))
            .stderr(Stdio::from(stderr_file))
            .spawn();
        let mut child = match child_result {
            Ok(child) => child,
            Err(err) => {
                app_log::error(format!(
                    "codex exec spawn failed cli={} error={err}",
                    codex_cli.display()
                ));
                cleanup_codex_files(&[&output_path, &stdout_path, &stderr_path, &schema_path]);
                return Err(err)
                    .with_context(|| format!("failed to execute `{}`", codex_cli.display()));
            }
        };

        if let Some(mut stdin) = child.stdin.take() {
            if let Err(err) = stdin.write_all(prompt.as_bytes()) {
                let _ = child.kill();
                let _ = child.wait();
                app_log::error(format!("codex exec stdin write failed error={err}"));
                cleanup_codex_files(&[&output_path, &stdout_path, &stderr_path, &schema_path]);
                return Err(err).context("failed to write prompt to Codex CLI");
            }
        }

        let deadline = SystemTime::now() + Duration::from_secs(LLM_REQUEST_TIMEOUT_SECONDS);
        let status = loop {
            if let Some(status) = child.try_wait().context("failed to wait for Codex CLI")? {
                break status;
            }
            if SystemTime::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                app_log::warn(format!(
                    "codex exec timed out timeout_seconds={LLM_REQUEST_TIMEOUT_SECONDS}"
                ));
                cleanup_codex_files(&[&output_path, &stdout_path, &stderr_path, &schema_path]);
                bail!("Codex CLI request timed out")
            }
            thread::sleep(Duration::from_millis(CODEX_POLL_INTERVAL_MS));
        };

        if let Some(text) = read_codex_final_text(&output_path, &stdout_path) {
            if !status.success() {
                app_log::warn(format!(
                    "codex exec completed nonzero but final response exists status={status} response_chars={}",
                    text.chars().count()
                ));
                write_codex_diagnostic_log(
                    status,
                    "Codex CLIは非ゼロ終了しましたが、構造化された最終応答を取得できたため採用しました。",
                    &format!(
                        "{}\n{}",
                        fs::read_to_string(&stderr_path).unwrap_or_default(),
                        fs::read_to_string(&stdout_path).unwrap_or_default()
                    ),
                );
            } else {
                app_log::info(format!(
                    "codex exec completed status={status} response_chars={}",
                    text.chars().count()
                ));
            }
            cleanup_codex_files(&[&output_path, &stdout_path, &stderr_path, &schema_path]);
            return Ok(strip_thinking_tags(&text));
        }

        if !status.success() {
            let diagnostic = codex_failure_diagnostic(status, &stdout_path, &stderr_path);
            app_log::warn(format!(
                "codex exec failed status={status} diagnostic={diagnostic}"
            ));
            cleanup_codex_files(&[&output_path, &stdout_path, &stderr_path, &schema_path]);
            bail!("{diagnostic}")
        }

        app_log::warn(format!(
            "codex exec completed status={status} empty_final_response"
        ));
        cleanup_codex_files(&[&output_path, &stdout_path, &stderr_path, &schema_path]);
        bail!("Codex CLI produced an empty final response")
    }
}

static CODEX_CLI_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

fn resolve_codex_cli() -> Option<PathBuf> {
    CODEX_CLI_PATH
        .get_or_init(|| {
            let mut candidates = Vec::new();
            candidates.push(PathBuf::from("codex"));
            candidates.extend(CODEX_CLI_FALLBACK_PATHS.iter().map(PathBuf::from));

            candidates.into_iter().find(|candidate| {
                Command::new(candidate)
                    .arg("--version")
                    .output()
                    .map(|output| output.status.success())
                    .unwrap_or(false)
            })
        })
        .clone()
}

fn codex_temp_path(suffix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    env::temp_dir().join(format!("mac-notify-codex-{nanos}-{suffix}"))
}

fn write_codex_schema_file() -> Result<PathBuf> {
    let path = codex_temp_path("schema.json");
    fs::write(&path, CODEX_ANALYSIS_OUTPUT_SCHEMA)
        .with_context(|| format!("failed to write Codex output schema at {}", path.display()))?;
    Ok(path)
}

fn cleanup_codex_files(paths: &[&Path]) {
    for path in paths {
        let _ = fs::remove_file(path);
    }
}

fn read_codex_final_text(output_path: &Path, stdout_path: &Path) -> Option<String> {
    for path in [output_path, stdout_path] {
        let text = fs::read_to_string(path).unwrap_or_default();
        let text = text.trim();
        if !text.is_empty() {
            return Some(text.to_string());
        }
    }
    None
}

pub(crate) enum CodexErrorCategory {
    Auth,
    RateLimit,
    Schema,
    Mcp,
    TrustedDirectory,
    EmptyResponse,
    NotAvailable,
    Unknown,
}

impl CodexErrorCategory {
    pub(crate) fn classify(text: &str) -> Self {
        let normalized = text.to_lowercase();
        if normalized.contains("not logged in")
            || normalized.contains("log in")
            || normalized.contains("login")
            || normalized.contains("auth")
            || normalized.contains("unauthorized")
            || normalized.contains("401")
        {
            Self::Auth
        } else if normalized.contains("rate limit")
            || normalized.contains("rate_limit")
            || normalized.contains("429")
        {
            Self::RateLimit
        } else if normalized.contains("output-schema") || normalized.contains("schema") {
            Self::Schema
        } else if normalized.contains("mcp") || normalized.contains("connector") {
            Self::Mcp
        } else if normalized.contains("trusted directory")
            || normalized.contains("skip-git-repo-check")
            || normalized.contains("not inside a trusted directory")
        {
            Self::TrustedDirectory
        } else if normalized.contains("not available") {
            Self::NotAvailable
        } else if normalized.contains("empty final response") {
            Self::EmptyResponse
        } else {
            Self::Unknown
        }
    }

    pub(crate) fn description(&self) -> &'static str {
        match self {
            Self::Auth => {
                "認証に失敗した可能性があります。`codex login` の状態を確認してください。"
            }
            Self::RateLimit => {
                "レート制限に達した可能性があります。少し待ってから再試行してください。"
            }
            Self::Schema => "構造化出力スキーマの適用に失敗した可能性があります。",
            Self::Mcp => "MCPまたはconnectorの初期化・実行に失敗した可能性があります。",
            Self::TrustedDirectory => "Codex CLIの作業ディレクトリ信頼チェックに失敗しました。",
            Self::NotAvailable => "Codex CLIを利用できませんでした。",
            Self::EmptyResponse => "Codex CLIの最終応答が空でした。",
            Self::Unknown => "詳細分類できない実行失敗です。",
        }
    }

    fn is_signal_keyword(lowered: &str) -> bool {
        lowered.contains("error")
            || lowered.contains("failed")
            || lowered.contains("panic")
            || lowered.contains("unauthorized")
            || lowered.contains("not logged in")
            || lowered.contains("login")
            || lowered.contains("auth")
            || lowered.contains("rate limit")
            || lowered.contains("429")
            || lowered.contains("schema")
            || lowered.contains("connector")
            || lowered.contains("trusted directory")
            || lowered.contains("skip-git-repo-check")
    }
}

fn codex_failure_diagnostic(status: ExitStatus, stdout_path: &Path, stderr_path: &Path) -> String {
    let stdout = fs::read_to_string(stdout_path).unwrap_or_default();
    let stderr = fs::read_to_string(stderr_path).unwrap_or_default();
    let combined = format!("{stderr}\n{stdout}");
    let category = CodexErrorCategory::classify(&combined);

    let log_path = write_codex_diagnostic_log(status, category.description(), &combined);
    format!(
        "Codex CLIの実行に失敗しました（status: {status}）。{} 診断ログ: {}",
        category.description(),
        log_path.display()
    )
}

fn write_codex_diagnostic_log(status: ExitStatus, category: &str, stderr: &str) -> PathBuf {
    let log_path = codex_diagnostics_log_path();
    if let Some(parent) = log_path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let signal_lines = codex_stderr_signal_lines(stderr);
    let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let mut entry =
        format!("\n[{timestamp}] Codex CLI failed\nstatus: {status}\ncategory: {category}\n");
    if signal_lines.is_empty() {
        entry.push_str("signals: (no safe diagnostic lines captured)\n");
    } else {
        entry.push_str("signals:\n");
        for line in signal_lines {
            entry.push_str("- ");
            entry.push_str(&line);
            entry.push('\n');
        }
    }

    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&log_path) {
        let _ = file.write_all(entry.as_bytes());
    }
    log_path
}

fn codex_stderr_signal_lines(stderr: &str) -> Vec<String> {
    let mut in_user_prompt = false;
    let mut signals = Vec::new();

    for line in stderr.lines() {
        let trimmed = line.trim();
        if trimmed == "user" {
            in_user_prompt = true;
            continue;
        }

        let starts_with_timestamp = trimmed.chars().take(4).all(|ch| ch.is_ascii_digit())
            && trimmed.contains('T')
            && trimmed.contains('Z');
        if in_user_prompt && !starts_with_timestamp {
            continue;
        }
        if starts_with_timestamp {
            in_user_prompt = false;
        }

        if let Some(signal) = codex_stderr_signal_line(trimmed) {
            signals.push(signal);
        }
    }

    signals
        .into_iter()
        .rev()
        .take(80)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn codex_stderr_signal_line(line: &str) -> Option<String> {
    let lowered = line.to_lowercase();
    let starts_with_timestamp = line
        .chars()
        .next()
        .map(|ch| ch.is_ascii_digit())
        .unwrap_or(false)
        && line.contains('T')
        && line.contains('Z');

    if line.starts_with("mcp:") || starts_with_timestamp {
        let is_signal = line.starts_with("mcp:") || CodexErrorCategory::is_signal_keyword(&lowered);
        return is_signal.then(|| redact_codex_diagnostic_line(line));
    }

    match CodexErrorCategory::classify(line) {
        CodexErrorCategory::Auth => Some("[codex error signal: auth/login]".to_string()),
        CodexErrorCategory::RateLimit => Some("[codex error signal: rate-limit]".to_string()),
        CodexErrorCategory::Schema => Some("[codex error signal: schema]".to_string()),
        CodexErrorCategory::Mcp => Some("[codex error signal: mcp/connector]".to_string()),
        CodexErrorCategory::TrustedDirectory => {
            Some("[codex error signal: trusted-directory]".to_string())
        }
        _ => None,
    }
}

fn redact_codex_diagnostic_line(line: &str) -> String {
    let mut redacted = line.to_string();
    if let Ok(home) = env::var("HOME") {
        redacted = redacted.replace(&home, "$HOME");
    }
    truncate_chars(&redacted, 240)
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

pub fn build_analysis_prompt(
    notification: &Notification,
    app_context: Option<&str>,
    user_context: Option<&str>,
) -> String {
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
外部コンテキストの扱い:\\n\
- 利用可能な読み取り専用のMCP、connector、plugin、toolがあり、通知本文だけでは判定に不足がある場合は、判定に必要な最小範囲で参照してよい\\n\
- 例: Slackの該当チャンネル/スレッドの直近数件、GitHubの該当PR/issue、Calendarの該当予定、Gmailの該当スレッドなど\\n\
- 書き込み、送信、下書き、リアクション、予定作成、ファイル変更など状態を変える操作は禁止\\n\
- 参照できない場合や該当先を安全に特定できない場合は、通知本文だけで判定する\\n\
- 外部コンテキストの本文、ID、個人情報、機密情報をsummary_lineやreasonにそのまま引用しない。必要な根拠だけを要約する\\n\
- 外部コンテキスト取得そのものに時間をかけすぎず、緊急度判定に効く場合だけ使う\\n\\n\
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

    if let Some(ctx) = user_context {
        prompt.push_str(&format!(
            "\\n\\nユーザーコンテキスト（この通知の受信者に関する情報。緊急度判定の参考にしてください）:\\n{ctx}"
        ));
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
        build_analysis_prompt, build_prompt_notification_view, codex_failure_diagnostic,
        codex_stderr_signal_lines, read_codex_final_text, resolve_codex_cli,
        PromptNotificationKind, CODEX_ANALYSIS_OUTPUT_SCHEMA, CODEX_CLI_FALLBACK_PATHS,
        SLACK_BUNDLE_ID,
    };
    use crate::models::Notification;
    use serde_json::Value;

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
        assert!(view.detail_lines.contains(&"会話名: #ns_zatsu".to_string()));
        assert!(view
            .detail_lines
            .contains(&"送信者表示名: Jo Okazaki（ジョー）".to_string()));
        assert!(view
            .detail_lines
            .contains(&"メッセージ本文: ほしくなる".to_string()));
    }

    #[test]
    fn slack_integration_message_is_preprocessed_for_prompt() {
        let notification = sample_notification(
            "バクラク勤怠 からの新しいメッセージ",
            "勤怠エラーが4日分あります",
        );

        let view = build_prompt_notification_view(&notification);

        assert_eq!(
            view.kind,
            PromptNotificationKind::SlackIntegrationNotification
        );
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

        let prompt =
            build_analysis_prompt(&notification, Some("Slackワークスペースの社内連絡"), None);

        assert!(prompt.contains("タイトル: #ns_zatsu の新しいメッセージ"));
        assert!(prompt.contains("本文: Jo Okazaki（ジョー）: ほしくなる"));
        assert!(prompt.contains("通知種別: slack_channel_message"));
        assert!(prompt.contains("送信者表示名: Jo Okazaki（ジョー）"));
        assert!(prompt.contains("メッセージ本文: ほしくなる"));
        assert!(
            prompt.contains("このアプリに関する追加コンテキスト: Slackワークスペースの社内連絡")
        );
    }

    #[test]
    fn prompt_allows_bounded_read_only_external_context() {
        let notification = sample_notification(
            "#incident の新しいメッセージ",
            "Ops Bot: 本番APIのエラー率が急上昇しています",
        );

        let prompt = build_analysis_prompt(&notification, None, None);

        assert!(prompt.contains("利用可能な読み取り専用のMCP、connector、plugin、tool"));
        assert!(prompt.contains("Slackの該当チャンネル/スレッドの直近数件"));
        assert!(prompt.contains("GitHubの該当PR/issue"));
        assert!(prompt.contains("書き込み、送信、下書き、リアクション"));
        assert!(prompt.contains("通知本文だけで判定する"));
        assert!(prompt.contains("そのまま引用しない"));
    }

    #[test]
    fn codex_cli_fallback_paths_include_common_macos_locations() {
        assert!(CODEX_CLI_FALLBACK_PATHS.contains(&"/opt/homebrew/bin/codex"));
        assert!(CODEX_CLI_FALLBACK_PATHS.contains(&"/usr/local/bin/codex"));
        assert!(
            CODEX_CLI_FALLBACK_PATHS.contains(&"/Applications/Codex.app/Contents/Resources/codex")
        );
    }

    #[test]
    fn codex_cli_resolves_when_available_in_development_environment() {
        if CODEX_CLI_FALLBACK_PATHS
            .iter()
            .any(|path| std::path::Path::new(path).exists())
        {
            assert!(resolve_codex_cli().is_some());
        }
    }

    #[test]
    fn codex_output_schema_requires_analysis_fields() {
        let schema: Value = serde_json::from_str(CODEX_ANALYSIS_OUTPUT_SCHEMA).unwrap();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["additionalProperties"], false);
        assert!(schema["required"]
            .as_array()
            .unwrap()
            .contains(&Value::String("summary_line".to_string())));
        assert!(schema["required"]
            .as_array()
            .unwrap()
            .contains(&Value::String("reason".to_string())));
        assert!(schema["required"]
            .as_array()
            .unwrap()
            .contains(&Value::String("urgency_level".to_string())));
    }

    #[cfg(unix)]
    #[test]
    fn codex_failure_diagnostic_classifies_auth_without_leaking_stderr() {
        use std::os::unix::process::ExitStatusExt;

        let stdout_path = std::env::temp_dir().join("mac-notify-codex-test-stdout.log");
        let stderr_path = std::env::temp_dir().join("mac-notify-codex-test-stderr.log");
        let log_path = std::env::temp_dir().join("mac-notify-codex-test-diagnostics.log");
        std::env::set_var("MAC_NOTIFY_CODEX_DIAGNOSTICS_LOG", &log_path);
        std::fs::write(&stdout_path, "").unwrap();
        std::fs::write(
            &stderr_path,
            "Error: authentication failed. Please login. SECRET_TEXT",
        )
        .unwrap();

        let diagnostic = codex_failure_diagnostic(
            std::process::ExitStatus::from_raw(1),
            &stdout_path,
            &stderr_path,
        );
        let _ = std::fs::remove_file(stdout_path);
        let _ = std::fs::remove_file(stderr_path);
        let log = std::fs::read_to_string(&log_path).unwrap();
        let _ = std::fs::remove_file(&log_path);
        std::env::remove_var("MAC_NOTIFY_CODEX_DIAGNOSTICS_LOG");

        assert!(diagnostic.contains("認証"));
        assert!(!diagnostic.contains("SECRET_TEXT"));
        assert!(!log.contains("SECRET_TEXT"));
    }

    #[test]
    fn codex_stderr_signal_lines_skip_prompt_body() {
        let lines = codex_stderr_signal_lines(
            "user\n通知本文: SECRET_NOTIFICATION error\n2026-06-23T00:00:00Z  WARN schema failed\nmcp: codex_apps/slack.read started\nError: schema failed SECRET_ERROR\n",
        );

        assert!(lines.iter().any(|line| line.contains("mcp:")));
        assert!(lines.iter().any(|line| line.contains("schema failed")));
        assert!(lines
            .iter()
            .any(|line| line.contains("[codex error signal: schema]")));
        assert!(!lines
            .iter()
            .any(|line| line.contains("SECRET_NOTIFICATION")));
        assert!(!lines.iter().any(|line| line.contains("SECRET_ERROR")));
    }

    #[test]
    fn codex_stderr_signal_lines_capture_trusted_directory_error() {
        let lines = codex_stderr_signal_lines(
            "Not inside a trusted directory and --skip-git-repo-check was not specified.",
        );

        assert!(lines.iter().any(|line| line.contains("trusted-directory")));
    }

    #[test]
    fn read_codex_final_text_falls_back_to_stdout() {
        let output_path = std::env::temp_dir().join("mac-notify-codex-test-output.json");
        let stdout_path = std::env::temp_dir().join("mac-notify-codex-test-stdout.json");
        let expected = "{\"summary_line\":\"x\",\"reason\":\"y\",\"urgency_level\":\"low\"}";

        std::fs::write(&output_path, "").unwrap();
        std::fs::write(&stdout_path, expected).unwrap();

        let actual = read_codex_final_text(&output_path, &stdout_path);
        let _ = std::fs::remove_file(output_path);
        let _ = std::fs::remove_file(stdout_path);

        assert_eq!(actual.as_deref(), Some(expected));
    }
}
