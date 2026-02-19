use std::env;
use std::io::Cursor;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use log::warn;
use plist::Value as PlistValue;
use rusqlite::{params, Connection, OpenFlags};

use crate::models::{Notification, ParsedPlist};

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

pub struct NotificationDb {
    db_path: PathBuf,
    query: Option<&'static str>,
}

impl NotificationDb {
    pub fn new(db_path: PathBuf) -> Self {
        Self {
            db_path,
            query: None,
        }
    }

    pub fn read_new(&mut self, since_rowid: i64) -> Result<Vec<Notification>> {
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

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

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
                timestamp: now,
            });
        }

        Ok(notifications)
    }

    pub fn latest_rowid(&mut self) -> Result<i64> {
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

pub fn get_notification_db_path() -> Result<PathBuf> {
    let major = macos_major_version();
    if major < 15 {
        bail!("notify supports macOS 15 (Tahoe) or newer only. detected major: {major}");
    }

    let home = env::var("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Group Containers")
        .join("group.com.apple.usernoted")
        .join("db2")
        .join("db"))
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
