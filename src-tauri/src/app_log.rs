use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

static LOG_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

pub fn path() -> PathBuf {
    if let Ok(path) = env::var("MAC_NOTIFY_APP_LOG") {
        return PathBuf::from(path);
    }

    env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".config/notify/app.log")
}

pub fn info(message: impl AsRef<str>) {
    write("INFO", message.as_ref());
}

pub fn warn(message: impl AsRef<str>) {
    write("WARN", message.as_ref());
}

pub fn error(message: impl AsRef<str>) {
    write("ERROR", message.as_ref());
}

fn write(level: &str, message: &str) {
    let lock = LOG_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock.lock().ok();

    let log_path = path();
    if let Some(parent) = log_path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let sanitized = sanitize(message);
    let line = format!("[{timestamp}] {level} {sanitized}\n");

    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(log_path) {
        let _ = file.write_all(line.as_bytes());
    }
}

fn sanitize(message: &str) -> String {
    let mut text = message.replace('\n', " / ").replace('\r', " ");
    if let Ok(home) = env::var("HOME") {
        text = text.replace(&home, "$HOME");
    }
    truncate_chars(&text, 800)
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::{info, path};

    #[test]
    fn app_log_uses_override_path_and_redacts_home() {
        let log_path = std::env::temp_dir().join("mac-notify-app-log-test.log");
        let _ = std::fs::remove_file(&log_path);
        std::env::set_var("MAC_NOTIFY_APP_LOG", &log_path);

        let home = std::env::var("HOME").unwrap_or_default();
        info(format!("test path {home}/Library/DoNotDisturb"));

        let written = std::fs::read_to_string(path()).unwrap();
        let _ = std::fs::remove_file(&log_path);
        std::env::remove_var("MAC_NOTIFY_APP_LOG");

        assert!(written.contains("INFO"));
        assert!(written.contains("$HOME/Library/DoNotDisturb"));
        assert!(!home.is_empty());
        assert!(!written.contains(&home));
    }
}
