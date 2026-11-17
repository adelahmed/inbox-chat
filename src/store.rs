//! Persistent storage in one SQLite file (default `%APPDATA%\InboxChat\inbox-chat.db`):
//! settings, encrypted API keys, conversations, the short-id registry and a log of saved drafts.

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS secrets (name TEXT PRIMARY KEY, blob BLOB NOT NULL);
CREATE TABLE IF NOT EXISTS conversations (
    id TEXT PRIMARY KEY, title TEXT NOT NULL, created TEXT NOT NULL, updated TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS messages (
    conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL, body TEXT NOT NULL, PRIMARY KEY (conversation_id, seq));
CREATE TABLE IF NOT EXISTS email_ids (n INTEGER PRIMARY KEY AUTOINCREMENT, entry_id TEXT NOT NULL UNIQUE);
CREATE TABLE IF NOT EXISTS drafts_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT, created TEXT NOT NULL, conversation_id TEXT,
    kind TEXT NOT NULL, short_id TEXT, subject TEXT, recipients TEXT);
CREATE TABLE IF NOT EXISTS sent_seen (sent_id TEXT PRIMARY KEY);
CREATE TABLE IF NOT EXISTS reply_examples (
    sent_id TEXT PRIMARY KEY, sent_at TEXT, recipients TEXT, subject TEXT,
    incoming_from TEXT, incoming_text TEXT, reply_text TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS insights (entry_id TEXT PRIMARY KEY, created TEXT NOT NULL, data TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS contacts (
    email TEXT PRIMARY KEY, name TEXT NOT NULL, count INTEGER NOT NULL DEFAULT 0, last TEXT);
CREATE TABLE IF NOT EXISTS contact_seen (item_id TEXT PRIMARY KEY);
CREATE TABLE IF NOT EXISTS followups (
    entry_id TEXT PRIMARY KEY, sent_at TEXT, subject TEXT, recipients TEXT NOT NULL, asked TEXT,
    is_request INTEGER NOT NULL, status TEXT NOT NULL DEFAULT 'open', closed_by TEXT, closed_at TEXT,
    reply_entry_id TEXT, you_followed_up_at TEXT, others_replied TEXT, dismissed INTEGER NOT NULL DEFAULT 0,
    checked_at TEXT);
"#;

/// A sent email the user may need to follow up on (see `followup.rs`).
#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq, Default)]
pub struct FollowupRow {
    pub entry_id: String,
    pub sent_at: Option<String>,
    pub subject: String,
    /// To recipients as (display name, address).
    pub recipients: Vec<(String, String)>,
    pub asked: String,
    pub is_request: bool,
    /// "open" or "closed"
    pub status: String,
    pub closed_by: Option<String>,
    pub closed_at: Option<String>,
    pub reply_entry_id: Option<String>,
    pub you_followed_up_at: Option<String>,
    pub others_replied: Vec<String>,
    pub dismissed: bool,
}

/// Someone the user exchanges mail with (from Sent Items recipients and Inbox senders).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Contact {
    pub name: String,
    pub email: String,
    pub count: i64,
    pub last: Option<String>,
}

/// One of the user's past replies: what came in, and what they answered.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Example {
    pub sent_id: String,
    pub sent_at: Option<String>,
    pub recipients: String,
    pub subject: String,
    pub incoming_from: String,
    pub incoming_text: String,
    pub reply_text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConversationInfo {
    pub id: String,
    pub title: String,
    pub updated: String,
}

pub struct Store {
    conn: Mutex<Connection>,
    pub path: PathBuf,
}

pub type DbResult<T> = Result<T, rusqlite::Error>;

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string()
}

impl Store {
    pub fn default_path() -> PathBuf {
        if let Ok(p) = std::env::var("INBOX_CHAT_DB") {
            return PathBuf::from(p);
        }
        let base = std::env::var("APPDATA").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."));
        base.join("InboxChat").join("inbox-chat.db")
    }

    pub fn open(path: &Path) -> DbResult<Self> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Store { conn: Mutex::new(conn), path: path.to_path_buf() })
    }

    /// Fallback when the database file cannot be opened; also used by tests.
    pub fn memory() -> DbResult<Self> {
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Store { conn: Mutex::new(conn), path: PathBuf::from(":memory:") })
    }

    #[cfg(test)]
    pub fn in_memory() -> Self {
        Self::memory().unwrap()
    }

    fn db(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    // -- key/value + secrets ------------------------------------------------------
    pub fn kv_get(&self, key: &str) -> DbResult<Option<String>> {
        self.db().query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0)).optional()
    }

    pub fn kv_set(&self, key: &str, value: &str) -> DbResult<()> {
        self.db().execute(
            "INSERT INTO kv (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn secret_get(&self, name: &str) -> DbResult<Option<Vec<u8>>> {
        self.db().query_row("SELECT blob FROM secrets WHERE name = ?1", [name], |r| r.get(0)).optional()
    }

    pub fn secret_set(&self, name: &str, blob: &[u8]) -> DbResult<()> {
        self.db().execute(
            "INSERT INTO secrets (name, blob) VALUES (?1, ?2) ON CONFLICT(name) DO UPDATE SET blob = excluded.blob",
            params![name, blob],
        )?;
        Ok(())
    }

    pub fn secret_delete(&self, name: &str) -> DbResult<()> {
        self.db().execute("DELETE FROM secrets WHERE name = ?1", [name])?;
        Ok(())
    }

    // -- short ids (m1, m2, ...) ↔ Outlook EntryIDs, stable across runs -------------
    pub fn short_id(&self, entry_id: &str) -> DbResult<String> {
        let db = self.db();
        // Look up first: an ignored INSERT would still burn an AUTOINCREMENT number, and the list
        // pane re-registers every email on each refresh.
        let existing: Option<i64> = db.query_row("SELECT n FROM email_ids WHERE entry_id = ?1", [entry_id], |r| r.get(0)).optional()?;
        let n = match existing {
            Some(n) => n,
            None => {
                db.execute("INSERT INTO email_ids (entry_id) VALUES (?1)", [entry_id])?;
                db.last_insert_rowid()
            }
        };
        Ok(format!("m{n}"))
    }

    pub fn entry_id(&self, short: &str) -> DbResult<Option<String>> {
        let Some(n) = short.strip_prefix('m').and_then(|d| d.parse::<i64>().ok()) else { return Ok(None) };
        self.db().query_row("SELECT entry_id FROM email_ids WHERE n = ?1", [n], |r| r.get(0)).optional()
    }

    // -- conversations ------------------------------------------------------------------
    pub fn create_conversation(&self, title: &str) -> DbResult<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let t = now();
        self.db().execute(
            "INSERT INTO conversations (id, title, created, updated) VALUES (?1, ?2, ?3, ?3)",
            params![id, title, t],
        )?;
        Ok(id)
    }

    pub fn conversation_exists(&self, id: &str) -> DbResult<bool> {
        Ok(self.db().query_row("SELECT 1 FROM conversations WHERE id = ?1", [id], |_| Ok(())).optional()?.is_some())
    }

    pub fn list_conversations(&self, limit: usize) -> DbResult<Vec<ConversationInfo>> {
        let db = self.db();
        let mut st = db.prepare("SELECT id, title, updated FROM conversations ORDER BY updated DESC LIMIT ?1")?;
        let rows = st.query_map([limit as i64], |r| Ok(ConversationInfo { id: r.get(0)?, title: r.get(1)?, updated: r.get(2)? }))?;
        rows.collect()
    }

    pub fn delete_conversation(&self, id: &str) -> DbResult<()> {
        self.db().execute("DELETE FROM conversations WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn load_history(&self, id: &str) -> DbResult<Vec<Value>> {
        let db = self.db();
        let mut st = db.prepare("SELECT body FROM messages WHERE conversation_id = ?1 ORDER BY seq")?;
        let rows = st.query_map([id], |r| r.get::<_, String>(0))?;
        Ok(rows.filter_map(|b| b.ok().and_then(|s| serde_json::from_str(&s).ok())).collect())
    }

    /// Append messages to a conversation in one transaction.
    pub fn append_messages(&self, id: &str, msgs: &[Value]) -> DbResult<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        let start: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM messages WHERE conversation_id = ?1", [id], |r| r.get(0))?;
        for (i, m) in msgs.iter().enumerate() {
            tx.execute(
                "INSERT INTO messages (conversation_id, seq, body) VALUES (?1, ?2, ?3)",
                params![id, start + 1 + i as i64, m.to_string()],
            )?;
        }
        tx.execute("UPDATE conversations SET updated = ?2 WHERE id = ?1", params![id, now()])?;
        tx.commit()
    }

    // -- audit log of drafts the assistant saved ---------------------------------------
    pub fn log_draft(&self, conversation_id: Option<&str>, kind: &str, short_id: &str, subject: &str, recipients: &str) -> DbResult<()> {
        self.db().execute(
            "INSERT INTO drafts_log (created, conversation_id, kind, short_id, subject, recipients) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![now(), conversation_id, kind, short_id, subject, recipients],
        )?;
        Ok(())
    }

    pub fn draft_log_count(&self) -> DbResult<i64> {
        self.db().query_row("SELECT COUNT(*) FROM drafts_log", [], |r| r.get(0))
    }

    // -- learning from Sent Items -----------------------------------------------------------
    /// Mark a sent item as processed; returns false if it was already seen.
    pub fn mark_seen(&self, sent_id: &str) -> DbResult<bool> {
        Ok(self.db().execute("INSERT OR IGNORE INTO sent_seen (sent_id) VALUES (?1)", [sent_id])? == 1)
    }

    pub fn is_seen(&self, sent_id: &str) -> DbResult<bool> {
        Ok(self.db().query_row("SELECT 1 FROM sent_seen WHERE sent_id = ?1", [sent_id], |_| Ok(())).optional()?.is_some())
    }

    pub fn add_example(&self, e: &Example) -> DbResult<()> {
        self.db().execute(
            "INSERT OR REPLACE INTO reply_examples (sent_id, sent_at, recipients, subject, incoming_from, incoming_text, reply_text)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![e.sent_id, e.sent_at, e.recipients, e.subject, e.incoming_from, e.incoming_text, e.reply_text],
        )?;
        Ok(())
    }

    pub fn example_count(&self) -> DbResult<i64> {
        self.db().query_row("SELECT COUNT(*) FROM reply_examples", [], |r| r.get(0))
    }

    /// Most recent first.
    pub fn examples(&self, limit: usize) -> DbResult<Vec<Example>> {
        let db = self.db();
        let mut st = db.prepare(
            "SELECT sent_id, sent_at, recipients, subject, incoming_from, incoming_text, reply_text
             FROM reply_examples ORDER BY sent_at DESC LIMIT ?1")?;
        let rows = st.query_map([limit as i64], |r| Ok(Example {
            sent_id: r.get(0)?, sent_at: r.get(1)?, recipients: r.get(2)?, subject: r.get(3)?,
            incoming_from: r.get(4)?, incoming_text: r.get(5)?, reply_text: r.get(6)?,
        }))?;
        rows.collect()
    }

    pub fn forget_learning(&self) -> DbResult<()> {
        self.db().execute_batch("DELETE FROM reply_examples; DELETE FROM sent_seen; DELETE FROM insights;")
    }

    // -- cached reading-pane insights (summary, actions, suggested reply) as JSON ---------------
    pub fn insight(&self, entry_id: &str) -> DbResult<Option<String>> {
        self.db().query_row("SELECT data FROM insights WHERE entry_id = ?1", [entry_id], |r| r.get(0)).optional()
    }

    pub fn set_insight(&self, entry_id: &str, data: &str) -> DbResult<()> {
        self.db().execute("INSERT OR REPLACE INTO insights (entry_id, created, data) VALUES (?1, ?2, ?3)",
                          params![entry_id, now(), data])?;
        Ok(())
    }

    // -- correspondents ----------------------------------------------------------------------------
    /// Count one appearance of a person in an item (once per item, so re-scans don't inflate counts).
    /// Returns true if this (item, person) pair was new.
    pub fn add_contact(&self, item_id: &str, name: &str, email: &str, when: Option<&str>) -> DbResult<bool> {
        let email = email.trim().to_lowercase();
        if !email.contains('@') {
            return Ok(false);
        }
        let mut db = self.db();
        let tx = db.transaction()?;
        let key = format!("{item_id}|{email}");
        let new = tx.execute("INSERT OR IGNORE INTO contact_seen (item_id) VALUES (?1)", [&key])? == 1;
        if new {
            tx.execute(
                "INSERT INTO contacts (email, name, count, last) VALUES (?1, ?2, 1, ?3)
                 ON CONFLICT(email) DO UPDATE SET count = count + 1,
                   name = CASE WHEN excluded.name <> '' THEN excluded.name ELSE name END,
                   last = MAX(COALESCE(last, ''), COALESCE(excluded.last, ''))",
                params![email, name.trim(), when],
            )?;
        }
        tx.commit()?;
        Ok(new)
    }

    // -- follow-ups --------------------------------------------------------------------------------
    pub fn followup_known(&self, entry_id: &str) -> DbResult<bool> {
        Ok(self.db().query_row("SELECT 1 FROM followups WHERE entry_id = ?1", [entry_id], |_| Ok(())).optional()?.is_some())
    }

    /// Insert or update (keeps the dismissed flag of an existing row).
    pub fn save_followup(&self, f: &FollowupRow) -> DbResult<()> {
        self.db().execute(
            "INSERT INTO followups (entry_id, sent_at, subject, recipients, asked, is_request, status, closed_by, closed_at,
                                    reply_entry_id, you_followed_up_at, others_replied, dismissed, checked_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT(entry_id) DO UPDATE SET status = excluded.status, closed_by = excluded.closed_by,
               closed_at = excluded.closed_at, reply_entry_id = excluded.reply_entry_id,
               you_followed_up_at = excluded.you_followed_up_at, others_replied = excluded.others_replied,
               checked_at = excluded.checked_at",
            params![f.entry_id, f.sent_at, f.subject, serde_json::to_string(&f.recipients).unwrap_or_default(), f.asked,
                    f.is_request as i64, f.status, f.closed_by, f.closed_at, f.reply_entry_id, f.you_followed_up_at,
                    serde_json::to_string(&f.others_replied).unwrap_or_default(), f.dismissed as i64, now()],
        )?;
        Ok(())
    }

    /// Requests sent on or after `since` (ISO), newest first, dismissed ones excluded.
    pub fn followups_since(&self, since: &str) -> DbResult<Vec<FollowupRow>> {
        let db = self.db();
        let mut st = db.prepare(
            "SELECT entry_id, sent_at, subject, recipients, asked, is_request, status, closed_by, closed_at, reply_entry_id,
                    you_followed_up_at, others_replied, dismissed
             FROM followups WHERE is_request = 1 AND dismissed = 0 AND sent_at >= ?1 ORDER BY sent_at DESC")?;
        let rows = st.query_map([since], |r| Ok(FollowupRow {
            entry_id: r.get(0)?, sent_at: r.get(1)?, subject: r.get(2)?,
            recipients: serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or_default(),
            asked: r.get(4)?, is_request: r.get::<_, i64>(5)? != 0, status: r.get(6)?, closed_by: r.get(7)?,
            closed_at: r.get(8)?, reply_entry_id: r.get(9)?, you_followed_up_at: r.get(10)?,
            others_replied: r.get::<_, Option<String>>(11)?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default(),
            dismissed: r.get::<_, i64>(12)? != 0,
        }))?;
        rows.collect()
    }

    pub fn set_followup_dismissed(&self, entry_id: &str, dismissed: bool) -> DbResult<bool> {
        Ok(self.db().execute("UPDATE followups SET dismissed = ?2 WHERE entry_id = ?1", params![entry_id, dismissed as i64])? == 1)
    }

    pub fn followup_dismissed(&self, entry_id: &str) -> DbResult<bool> {
        Ok(self.db().query_row("SELECT dismissed FROM followups WHERE entry_id = ?1", [entry_id], |r| r.get::<_, i64>(0))
            .optional()?.unwrap_or(0) != 0)
    }

    pub fn dismissed_followup_count(&self) -> DbResult<i64> {
        self.db().query_row("SELECT COUNT(*) FROM followups WHERE dismissed = 1", [], |r| r.get(0))
    }

    pub fn restore_all_followups(&self) -> DbResult<usize> {
        self.db().execute("UPDATE followups SET dismissed = 0 WHERE dismissed = 1", [])
    }

    pub fn contact_count(&self) -> DbResult<i64> {
        self.db().query_row("SELECT COUNT(*) FROM contacts", [], |r| r.get(0))
    }

    /// Most frequent first.
    pub fn contacts(&self, limit: usize) -> DbResult<Vec<Contact>> {
        let db = self.db();
        let mut st = db.prepare("SELECT name, email, count, last FROM contacts ORDER BY count DESC, last DESC LIMIT ?1")?;
        let rows = st.query_map([limit as i64], |r| Ok(Contact { name: r.get(0)?, email: r.get(1)?, count: r.get(2)?, last: r.get(3)? }))?;
        rows.collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn persists_across_reopen() {
        let dir = std::env::temp_dir().join(format!("inboxchat-store-{}", uuid::Uuid::new_v4()));
        let path = dir.join("t.db");
        {
            let s = Store::open(&path).unwrap();
            assert_eq!(s.short_id("AAA").unwrap(), "m1");
            assert_eq!(s.short_id("BBB").unwrap(), "m2");
            assert_eq!(s.short_id("AAA").unwrap(), "m1");
            let c = s.create_conversation("hello").unwrap();
            s.append_messages(&c, &[json!({"role": "user", "content": "hi"})]).unwrap();
            s.append_messages(&c, &[json!({"role": "assistant", "content": "yo"})]).unwrap();
            s.kv_set("k", "v").unwrap();
            s.secret_set("gemini", b"\x01\x02").unwrap();
        }
        let s = Store::open(&path).unwrap();
        assert_eq!(s.entry_id("m2").unwrap().as_deref(), Some("BBB"));
        assert_eq!(s.short_id("CCC").unwrap(), "m3");
        assert_eq!(s.entry_id("x9").unwrap(), None);
        let convs = s.list_conversations(10).unwrap();
        assert_eq!(convs.len(), 1);
        let h = s.load_history(&convs[0].id).unwrap();
        assert_eq!(h.len(), 2);
        assert_eq!(h[1]["content"], "yo");
        assert_eq!(s.kv_get("k").unwrap().as_deref(), Some("v"));
        assert_eq!(s.secret_get("gemini").unwrap().unwrap(), vec![1, 2]);
        s.delete_conversation(&convs[0].id).unwrap();
        assert!(s.load_history(&convs[0].id).unwrap().is_empty(), "messages cascade-deleted");
        drop(s);
        let _ = std::fs::remove_dir_all(dir);
    }
}
