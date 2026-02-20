use serde::Serialize;

#[derive(Debug, Clone)]
pub struct Notification {
    pub rowid: i64,
    pub title: String,
    pub body: String,
    pub subtitle: String,
    pub bundle_id: String,
    pub timestamp: i64,
}

#[derive(Debug, Clone)]
pub struct AnalyzedNotification {
    pub id: i64,
    pub title: String,
    pub body: String,
    pub subtitle: String,
    pub bundle_id: String,
    pub app_name: String,
    pub urgency: UrgencyLevel,
    pub summary_line: String,
    pub reason: String,
    pub timestamp: i64,
}

#[derive(Debug, Clone)]
pub struct NotificationAnalysis {
    pub urgency: UrgencyLevel,
    pub summary_line: String,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum UrgencyLevel {
    Critical,
    High,
    Medium,
    Low,
}

impl UrgencyLevel {
    pub fn label(self) -> &'static str {
        match self {
            Self::Critical => "URGENT",
            Self::High => "HIGH",
            Self::Medium => "NORMAL",
            Self::Low => "LOW",
        }
    }

    pub fn color(self) -> &'static str {
        match self {
            Self::Critical => "#ef4444",
            Self::High => "#f97316",
            Self::Medium => "#f59e0b",
            Self::Low => "#22c55e",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusState {
    Active,
    Inactive,
}

#[derive(Debug, Clone)]
pub struct ParsedPlist {
    pub title: String,
    pub body: String,
    pub subtitle: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UiNotification {
    pub id: i64,
    pub title: String,
    pub body: String,
    pub subtitle: String,
    pub bundle_id: String,
    pub app_name: String,
    pub urgency_level: UrgencyLevel,
    pub urgency_label: String,
    pub urgency_color: String,
    pub summary_line: String,
    pub reason: String,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UiNotificationGroup {
    pub bundle_id: String,
    pub app_name: String,
    pub icon_base64: Option<String>,
    pub notifications: Vec<UiNotification>,
}
