//! The mailbox interface the tools use. `outlook::OutlookMail` implements it over COM; the tests use
//! an in-memory fake. Nothing here can send mail: the only writes save drafts.

pub use crate::profile::DraftText;
use serde::Serialize;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MailError {
    /// Unknown email/folder: the message tells the model what to do instead.
    NotFound(String),
    /// Bad arguments from the model.
    Invalid(String),
    /// Outlook itself failed (not running, COM error, ...).
    Outlook(String),
}

impl fmt::Display for MailError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MailError::NotFound(m) | MailError::Invalid(m) | MailError::Outlook(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for MailError {}

pub type MailResult<T> = Result<T, MailError>;

#[derive(Debug, Clone, Serialize)]
pub struct Folder {
    pub path: String,
    pub unread: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub entry_id: String,
    pub kind: String,
    pub subject: String,
    pub from_name: String,
    pub from: String,
    pub received: Option<String>,
    pub unread: bool,
    pub importance: String,
    pub attachments: i64,
    pub preview: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// Relevance tags (to_you, cc_you, mentions_you, department, request), filled by `profile::classify`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    // Used for classification only; not sent to the model or the UI.
    #[serde(skip)]
    pub to: String,
    #[serde(skip)]
    pub cc: String,
    #[serde(skip)]
    pub body_head: String,
}

/// A sent email, for learning the user's reply style and who they write to.
#[derive(Debug, Clone)]
pub struct SentItem {
    pub entry_id: String,
    pub sent_at: Option<String>,
    pub to: String,
    pub subject: String,
    pub body: String,
    /// (display name, SMTP address) of every recipient.
    pub recipients: Vec<(String, String)>,
}

/// Someone who wrote to the user (an Inbox sender).
#[derive(Debug, Clone)]
pub struct Sender {
    pub item_id: String,
    pub name: String,
    pub email: String,
    pub when: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Message {
    pub entry_id: String,
    pub kind: String,
    pub subject: String,
    pub from_name: String,
    pub from: String,
    pub received: Option<String>,
    pub unread: bool,
    pub importance: String,
    pub attachments: i64,
    pub to: String,
    pub cc: String,
    pub body: String,
    pub attachment_names: Vec<String>,
    pub conversation: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ThreadMsg {
    pub entry_id: String,
    pub from_name: String,
    pub from: String,
    pub to: String,
    pub received: Option<String>,
    pub subject: String,
    pub body: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DraftInfo {
    pub entry_id: String,
    pub subject: String,
    pub to: String,
    pub modified: Option<String>,
    pub preview: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SavedDraft {
    pub entry_id: String,
    pub subject: String,
    pub to: String,
    pub cc: String,
}

/// Result of saving a forward or a new draft. `unresolved` names were not found (or were ambiguous)
/// in the address book; they stay in the draft as typed for the user to fix with Check Names.
#[derive(Debug, Clone, Serialize)]
pub struct ForwardDraft {
    pub entry_id: String,
    pub subject: String,
    /// "Name <smtp>" for resolved To recipients, "CC: Name <smtp>" for CC.
    pub recipients: Vec<String>,
    pub attachments: i64,
    pub unresolved: Vec<String>,
}

/// "Rania; Mona, Hassan Alattar" → ["Rania", "Mona", "Hassan Alattar"] (also the Arabic comma).
pub fn split_recipients(text: &str) -> Vec<String> {
    text.split([';', ',', '،', '\n']).map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect()
}

/// A node of the folder tree shown in the left pane. `path` is what the tools accept.
#[derive(Debug, Clone, Serialize)]
pub struct FolderNode {
    pub path: String,
    pub name: String,
    /// "inbox", "drafts", "sent", "deleted", "junk" or "outbox" for Outlook's default folders,
    /// found by EntryID (names are localized, e.g. "علبة الوارد").
    pub special: Option<String>,
    pub unread: Option<i64>,
    pub total: Option<i64>,
    pub children: Vec<FolderNode>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AttachmentInfo {
    /// 1-based index in the item's Attachments collection.
    pub index: i64,
    pub name: String,
    pub size: i64,
}

/// Something the user did with an email: a reply, forward or other message in Sent Items or Drafts.
#[derive(Debug, Clone, Serialize)]
pub struct RelatedItem {
    pub entry_id: String,
    /// "sent" or "drafts"
    pub folder: String,
    /// "reply", "forward" or "other"
    pub kind: String,
    pub subject: String,
    pub to: String,
    pub cc: String,
    pub when: Option<String>,
    pub preview: String,
}

/// A later message in a conversation.
#[derive(Debug, Clone, Serialize)]
pub struct ThreadEvent {
    pub entry_id: String,
    pub from_name: String,
    pub from: String,
    pub when: Option<String>,
}

/// The conversation topic of a subject: RE:/FW:/رد:/إعادة توجيه: prefixes removed (repeatedly).
pub fn topic_of(subject: &str) -> String {
    let mut t = subject.trim().to_string();
    loop {
        let lower = t.to_lowercase();
        match ["re:", "fw:", "fwd:", "رد:", "الرد:", "إعادة توجيه:", "اعادة توجيه:"].iter().find(|p| lower.starts_with(*p)) {
            Some(p) => t = t[p.len()..].trim().to_string(),
            None => return t,
        }
    }
}

/// What the user did with an email.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Related {
    /// Outlook's own record of the last action: "replied", "replied to all" or "forwarded".
    pub last_action: Option<String>,
    pub last_action_at: Option<String>,
    pub items: Vec<RelatedItem>,
}

/// "reply" / "forward" / "other" from a subject prefix (English and Arabic Outlook).
pub fn kind_from_subject(subject: &str) -> &'static str {
    let s = subject.trim_start().to_lowercase();
    if ["re:", "رد:", "الرد:"].iter().any(|p| s.starts_with(p)) {
        "reply"
    } else if ["fw:", "fwd:", "إعادة توجيه:", "اعادة توجيه:", "توجيه:"].iter().any(|p| s.starts_with(p)) {
        "forward"
    } else {
        "other"
    }
}

/// Outlook's PR_LAST_VERB_EXECUTED codes.
pub fn verb_name(code: i64) -> Option<&'static str> {
    match code {
        102 => Some("replied"),
        103 => Some("replied to all"),
        104 => Some("forwarded"),
        _ => None,
    }
}

/// Everything the reading pane shows for one email.
#[derive(Debug, Clone, Serialize)]
pub struct FullMessage {
    pub entry_id: String,
    pub kind: String,
    pub subject: String,
    pub from_name: String,
    pub from: String,
    pub to: String,
    pub cc: String,
    pub received: Option<String>,
    pub importance: String,
    pub unread: bool,
    pub is_draft: bool,
    pub html: String,
    pub text: String,
    pub attachments: Vec<AttachmentInfo>,
    /// Outlook's "You replied/forwarded on …" status, if any.
    pub last_action: Option<String>,
    pub last_action_at: Option<String>,
}

/// Limits mirrored from the Python version; they keep tool output small for local models.
pub const BODY_CHARS: usize = 6000;
pub const THREAD_CHARS: usize = 2500;
pub const THREAD_MAX: usize = 12;
pub const PREVIEW_CHARS: usize = 200;

pub trait MailStore: Send + Sync {
    // -- used by the LLM tools ----------------------------------------------------------
    fn list_folders(&self, max_depth: usize) -> MailResult<Vec<Folder>>;
    fn list_messages(&self, folder: &str, days: Option<i64>, unread_only: bool, limit: usize) -> MailResult<Vec<Summary>>;
    /// Keyword / sender / recipient search. An Arabic phrase that finds nothing is retried word by word.
    fn search(&self, q: &SearchQuery, folders: &[String], days: Option<i64>, limit: usize) -> MailResult<Vec<Summary>>;
    /// What the user did with an email: Outlook's last-action status plus replies/forwards/drafts in
    /// Sent Items and Drafts from the same conversation (read-only).
    fn related(&self, entry_id: &str) -> MailResult<Related>;
    /// Messages in the same conversation that arrived or were sent after this one (any folder except
    /// Drafts), oldest first. Used to tell whether a request the user sent has been answered.
    fn later_in_conversation(&self, entry_id: &str) -> MailResult<Vec<ThreadEvent>>;
    fn get_message(&self, entry_id: &str) -> MailResult<Message>;
    /// Conversation oldest first, quoted history removed, at most `max_messages` of `chars` each.
    fn get_thread(&self, entry_id: &str, max_messages: usize, chars: usize) -> MailResult<Vec<ThreadMsg>>;
    fn list_drafts(&self, limit: usize) -> MailResult<Vec<DraftInfo>>;
    /// Save a reply in Drafts. Never sends. The text is rendered with the reply's actual To recipients.
    fn create_reply_draft(&self, entry_id: &str, text: &DraftText, reply_all: bool) -> MailResult<SavedDraft>;
    /// Save a new message in Drafts. Never sends. Recipients are resolved against the address book
    /// like a forward's (addresses always resolve; ambiguous names stay as typed).
    fn create_new_draft(&self, to: &str, subject: &str, text: &DraftText, cc: &str) -> MailResult<ForwardDraft>;
    /// Save a forward (original body and attachments kept) in Drafts. Recipients are resolved against
    /// the address book; ambiguous names are never guessed. An empty body leaves the original untouched.
    /// Never sends.
    fn create_forward_draft(&self, entry_id: &str, to: &str, text: &DraftText, cc: &str) -> MailResult<ForwardDraft>;

    // -- profile and learning ---------------------------------------------------------------
    /// The mailbox owner's (display name, SMTP address).
    fn whoami(&self) -> MailResult<(String, String)>;
    /// Sent Items newest first: `limit` items after `offset`, bodies up to 8000 chars; bool = more exist.
    fn sent_items(&self, offset: usize, limit: usize) -> MailResult<(Vec<SentItem>, bool)>;
    /// Inbox senders, newest first (`limit` items after `offset`); bool = more exist.
    fn recent_senders(&self, offset: usize, limit: usize) -> MailResult<(Vec<Sender>, bool)>;

    // -- used by the three-pane UI ---------------------------------------------------------
    fn folder_tree(&self, max_depth: usize) -> MailResult<Vec<FolderNode>>;
    /// Newest first, `limit` items after skipping `offset`; the bool says whether more exist.
    fn page(&self, folder: &str, offset: usize, limit: usize) -> MailResult<(Vec<Summary>, bool)>;
    /// Full content for the reading pane. Does not mark the email as read.
    fn read_full(&self, entry_id: &str) -> MailResult<FullMessage>;
    /// Save attachment `index` (1-based) into `dir` and return the file path.
    fn save_attachment(&self, entry_id: &str, index: i64, dir: &std::path::Path) -> MailResult<std::path::PathBuf>;
    /// Open the item in Outlook's own window (e.g. to finish and send a draft by hand).
    fn display(&self, entry_id: &str) -> MailResult<()>;
    /// Mark an email as read in Outlook (approved by the user on 15-Nov-2026). Returns true if it was unread.
    fn mark_read(&self, entry_id: &str) -> MailResult<bool>;
    /// Look a name up in the address book (read-only): (display name, SMTP) if exactly one person matches.
    fn resolve_name(&self, name: &str) -> MailResult<Option<(String, String)>>;
}

/// Make an attachment name safe to use as a file name.
pub fn safe_file_name(name: &str) -> String {
    let cleaned: String = name.chars()
        .map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '_' } else { c })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').to_string();
    let cleaned = if cleaned.is_empty() { "attachment".to_string() } else { cleaned };
    // Keep the extension when shortening.
    if cleaned.chars().count() <= 120 {
        return cleaned;
    }
    let (stem, ext) = match cleaned.rfind('.') {
        Some(i) if cleaned.len() - i <= 10 => (&cleaned[..i], &cleaned[i..]),
        _ => (cleaned.as_str(), ""),
    };
    format!("{}{}", stem.chars().take(110).collect::<String>(), ext)
}

#[derive(Debug, Clone, Default)]
pub struct SearchQuery {
    pub query: String,
    pub sender: String,
    /// Part of a To/CC display name (Outlook stores names there, not addresses).
    pub recipient: String,
}

const HAMZA_FORMS: &[char] = &['ء', 'ئ', 'ؤ', 'أ', 'إ', 'آ'];

/// Words to retry with when an exact phrase finds nothing: words of 3+ letters, skipping Arabic words
/// whose hamza is spelled differently by different people (موائمة / مواءمة).
pub fn fallback_words(query: &str) -> Vec<String> {
    let words: Vec<&str> = query.split_whitespace().filter(|w| w.chars().count() >= 3).collect();
    if words.len() < 2 {
        return vec![];
    }
    let stable: Vec<String> = words.iter().filter(|w| !w.contains(HAMZA_FORMS)).map(|w| w.to_string()).collect();
    if stable.is_empty() { words.iter().map(|w| w.to_string()).collect() } else { stable }
}

/// Build the DASL filter (`'` escaped as `''`). `words`: match all of them instead of the exact phrase.
pub fn dasl_filter(q: &SearchQuery, words: Option<&[String]>) -> MailResult<String> {
    let esc = |s: &str| s.replace('\'', "''");
    let like_any = |fields: &[&str], v: &str| {
        let ors: Vec<String> = fields.iter().map(|f| format!("\"urn:schemas:httpmail:{f}\" LIKE '%{}%'", esc(v))).collect();
        format!("({})", ors.join(" OR "))
    };
    let mut clauses = Vec::new();
    match words {
        Some(ws) => ws.iter().for_each(|w| clauses.push(like_any(&["subject", "textdescription", "fromname"], w))),
        None if !q.query.trim().is_empty() => clauses.push(like_any(&["subject", "textdescription", "fromname"], q.query.trim())),
        None => {}
    }
    if !q.sender.trim().is_empty() {
        clauses.push(like_any(&["fromname", "fromemail"], q.sender.trim()));
    }
    if !q.recipient.trim().is_empty() {
        clauses.push(like_any(&["displayto", "displaycc"], q.recipient.trim()));
    }
    if clauses.is_empty() {
        return Err(MailError::Invalid("Give a search query, a sender and/or a recipient.".into()));
    }
    Ok(format!("@SQL={}", clauses.join(" AND ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_recipients() {
        assert_eq!(split_recipients("Rania; Mona, Hassan Alattar،علي\n x "), ["Rania", "Mona", "Hassan Alattar", "علي", "x"]);
        assert!(split_recipients(" ; , ").is_empty());
    }

    #[test]
    fn file_names_are_sanitized() {
        assert_eq!(safe_file_name("a/b:c?.pptx"), "a_b_c_.pptx");
        assert_eq!(safe_file_name("  ..  "), "attachment");
        assert_eq!(safe_file_name("..\\..\\evil.exe"), "_.._evil.exe");
        let long = format!("{}.docx", "x".repeat(300));
        let s = safe_file_name(&long);
        assert!(s.ends_with(".docx") && s.chars().count() <= 120);
    }

    #[test]
    fn dasl_escapes_quotes_and_requires_terms() {
        let q = |query: &str, sender: &str, recipient: &str| SearchQuery { query: query.into(), sender: sender.into(), recipient: recipient.into() };
        let f = dasl_filter(&q("O'Neil", "", ""), None).unwrap();
        assert!(f.starts_with("@SQL=(") && f.contains("'%O''Neil%'"));
        assert!(dasl_filter(&q("", "", ""), None).is_err());
        assert!(dasl_filter(&q("a", "b", ""), None).unwrap().contains(" AND "));
        assert!(dasl_filter(&q("", "", "Mona"), None).unwrap().contains("\"urn:schemas:httpmail:displayto\" LIKE '%Mona%'"));
        assert_eq!(fallback_words("موائمة محتوى ساعة المعرفة"), ["محتوى", "ساعة", "المعرفة"], "hamza word skipped");
        assert!(fallback_words("budget").is_empty());
        assert_eq!(kind_from_subject("RE: x"), "reply");
        assert_eq!(kind_from_subject("إعادة توجيه: x"), "forward");
        assert_eq!(verb_name(104), Some("forwarded"));
    }
}
