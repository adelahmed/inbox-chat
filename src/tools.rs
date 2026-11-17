//! Tool definitions the LLM can call, plus the dispatcher.
//!
//! Outlook EntryIDs are ~140-character hex strings that models mangle when copying them back, so the
//! model sees short ids ("m1", "m2", ...). The mapping lives in SQLite, so ids stay valid across runs.

use crate::context::UserContext;
use crate::mail::{MailError, MailStore};
use crate::store::Store;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::sync::Arc;

fn func(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": {"type": "object", "properties": properties, "required": required},
        }
    })
}

pub fn tool_specs() -> Vec<Value> {
    let id = json!({"type": "string", "description": "Short email id such as 'm3', from a previous tool result"});
    let body = json!({"type": "string", "description": "The message only: no greeting, no sign-off (the app adds them)"});
    let greeting = json!({"type": "string", "description": "For one recipient: how the user addresses them in Arabic, e.g. 'الأستاذ خالد'"});
    vec![
        func("list_emails",
             "List recent emails in a folder, newest first. Use for 'what came in today', 'unread', 'this week'.",
             json!({
                 "folder": {"type": "string", "description": "inbox (default), sent, drafts, or a path like 'Inbox/Projects'"},
                 "days": {"type": "integer", "description": "Only emails from the last N days (default 7)"},
                 "unread_only": {"type": "boolean", "description": "Only unread emails"},
                 "limit": {"type": "integer", "description": "Max results, 1-50 (default 20)"},
             }), &[]),
        func("search_emails",
             "Search emails by keyword (subject, body, sender name), sender and/or recipient. Searches Inbox and Sent by default.",
             json!({
                 "query": {"type": "string", "description": "Keyword or phrase, any language"},
                 "sender": {"type": "string", "description": "Part of the sender's name or address (in Sent Items the sender is the user — use recipient)"},
                 "recipient": {"type": "string", "description": "Part of a To/CC person's display name, e.g. 'Mona' or 'منى'"},
                 "days": {"type": "integer", "description": "Only the last N days"},
                 "folders": {"type": "array", "items": {"type": "string"}, "description": "Folders to search"},
                 "limit": {"type": "integer", "description": "Max results, 1-50 (default 15)"},
             }), &[]),
        func("read_email", "Read the full content of one email (default: the email open in the reading pane).", json!({"id": id}), &["id"]),
        func("read_thread",
             "Read the whole conversation an email belongs to, oldest first, quoted history removed. \
              Always do this before drafting a reply.",
             json!({"id": id}), &["id"]),
        func("find_related",
             "What the user already did with an email: replies, forwards and drafts of it in Sent Items/Drafts (with \
              recipients and dates), and Outlook's own 'You replied/forwarded on…' status. Use for 'did I reply / \
              forward / take any action on this?' (default: the open email).",
             json!({"id": id}), &["id"]),
        func("find_followups",
             "Requests the user sent (Sent Items) that the person asked hasn't answered yet. \
              Use for 'what do I need to follow up' / 'ما الذي يحتاج متابعة'.",
             json!({"days": {"type": "integer", "description": "Look back N days (default 10)"}}), &[]),
        func("list_folders", "List mail folders with unread counts.", json!({}), &[]),
        func("create_reply_draft",
             "Save a reply to an email in the Drafts folder (it is NOT sent). The original is quoted below automatically.",
             json!({
                 "id": id,
                 "body": body.clone(),
                 "greeting_name": greeting.clone(),
                 "reply_all": {"type": "boolean", "description": "Reply to all recipients (default false)"},
             }), &["id", "body"]),
        func("create_forward_draft",
             "Save a forward of an email in the Drafts folder (it is NOT sent). The original and its attachments \
              are included automatically. Use when the user says forward / أعد توجيه / حوّل.",
             json!({
                 "id": id,
                 "to": {"type": "string", "description": "Email addresses (or names), separated by ';'"},
                 "body": {"type": "string", "description": "Message placed above the forwarded email (no greeting/sign-off)"},
                 "greeting_name": greeting.clone(),
                 "cc": {"type": "string", "description": "Optional CC addresses, separated by ';'"},
             }), &["id", "to"]),
        func("create_new_draft", "Save a new email in the Drafts folder (it is NOT sent).",
             json!({
                 "to": {"type": "string", "description": "Email addresses (or names), separated by ';'"},
                 "subject": {"type": "string"},
                 "body": body,
                 "greeting_name": greeting,
                 "cc": {"type": "string", "description": "Optional CC addresses, separated by ';'"},
             }), &["to", "subject", "body"]),
        func("find_recipient",
             "Look up a person's email address by name (Arabic or English) among people the user emails. \
              Use it before creating a draft when you only have a name.",
             json!({"name": {"type": "string", "description": "Name as the user said it, e.g. 'Mona' or 'منى'"}}), &["name"]),
        func("list_drafts", "List the most recent drafts.", json!({"limit": {"type": "integer"}}), &[]),
    ]
}

/// Lenient integer argument: accepts numbers and numeric strings, clamps to [lo, hi].
fn clamp(v: Option<&Value>, lo: i64, hi: i64, default: i64) -> i64 {
    let n = match v {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Some(Value::String(s)) => s.trim().parse::<f64>().ok().map(|f| f as i64),
        _ => None,
    };
    n.map(|n| n.clamp(lo, hi)).unwrap_or(default)
}

fn boolean(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => matches!(s.trim().to_lowercase().as_str(), "true" | "1" | "yes"),
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0) != 0.0,
        _ => false,
    }
}

fn string(a: &Value, key: &str) -> String {
    match a.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn opt(a: &Value, key: &str) -> Option<String> {
    Some(string(a, key).trim().to_string()).filter(|s| !s.is_empty())
}

pub struct MailTools {
    pub mail: Arc<dyn MailStore>,
    pub store: Arc<Store>,
    pub ctx: Arc<UserContext>,
}

impl MailTools {
    pub fn new(mail: Arc<dyn MailStore>, store: Arc<Store>) -> Self {
        let ctx = Arc::new(UserContext::load(store.clone(), mail.clone()));
        MailTools { mail, store, ctx }
    }

    /// The assistant read or acted on this email: mark it read in Outlook if the user wants that.
    pub fn processed(&self, entry_id: &str) {
        if self.ctx.profile().mark_read {
            let _ = self.mail.mark_read(entry_id);
        }
    }

    fn short(&self, entry_id: &str) -> Result<String, String> {
        self.store.short_id(entry_id).map_err(|e| format!("id registry: {e}"))
    }

    /// Short id (or a raw EntryID) → EntryID.
    pub fn long(&self, short: &Value) -> Result<String, String> {
        let raw = match short { Value::String(s) => s.clone(), other => other.to_string() };
        let sid = raw.trim().trim_matches(['[', ']']).to_lowercase();
        if let Some(e) = self.store.entry_id(&sid).map_err(|e| format!("id registry: {e}"))? {
            return Ok(e);
        }
        if sid.len() > 40 {
            return Ok(raw.trim().to_string()); // the model passed a raw EntryID
        }
        Err(format!("Unknown email id '{raw}'. Use an id returned by list/search tools."))
    }

    /// Serialize rows and swap `entry_id` for a short `id`.
    pub fn shorten<T: Serialize>(&self, rows: &[T]) -> Result<Vec<Value>, String> {
        rows.iter().map(|r| {
            let mut v = serde_json::to_value(r).map_err(|e| e.to_string())?;
            if let Some(obj) = v.as_object_mut() {
                if let Some(Value::String(eid)) = obj.remove("entry_id") {
                    obj.insert("id".into(), json!(self.short(&eid)?));
                }
            }
            Ok(v)
        }).collect()
    }

    /// Run a tool and return a JSON string. Errors are returned, not raised, so the model can recover.
    pub fn call(&self, name: &str, args: &Value, conversation_id: Option<&str>) -> String {
        self.call_with(name, args, conversation_id, None)
    }

    /// Like `call`, with the short id of the email open in the reading pane: tools that act on one
    /// email use it when the model gives no id (or "this"/"current").
    pub fn call_with(&self, name: &str, args: &Value, conversation_id: Option<&str>, viewing: Option<&str>) -> String {
        let empty = Value::Object(Map::new());
        let args = if args.is_object() { args } else { &empty };
        let result = self.dispatch(name, args, conversation_id, viewing).unwrap_or_else(|e| json!({"error": e}));
        result.to_string()
    }

    /// The email a tool should act on: the given id, or the open email when none (or a generic one) is given.
    fn target(&self, a: &Value, viewing: Option<&str>) -> Result<String, String> {
        let given = match a.get("id") {
            Some(Value::String(s)) => s.trim().to_lowercase(),
            Some(Value::Null) | None => String::new(),
            Some(other) => other.to_string(),
        };
        let generic = given.is_empty() || ["this", "current", "open", "opened", "this email", "current email", "the open email"]
            .contains(&given.trim_matches(['[', ']']));
        match (generic, viewing) {
            (true, Some(v)) => self.long(&json!(v)),
            _ => self.long(a.get("id").unwrap_or(&Value::Null)),
        }
    }

    fn dispatch(&self, name: &str, a: &Value, conversation_id: Option<&str>, viewing: Option<&str>) -> Result<Value, String> {
        let m = &self.mail;
        let mail_err = |e: MailError| e.to_string();
        match name {
            "list_emails" => {
                let folder = Some(string(a, "folder")).filter(|f| !f.is_empty()).unwrap_or_else(|| "inbox".into());
                let days = clamp(a.get("days"), 0, 365, 7);
                let mut rows = m.list_messages(&folder, (days > 0).then_some(days), boolean(a.get("unread_only")),
                                               clamp(a.get("limit"), 1, 50, 20) as usize).map_err(mail_err)?;
                self.ctx.tag(&mut rows);
                Ok(json!({"count": rows.len(), "emails": self.shorten(&rows)?}))
            }
            "search_emails" => {
                let folders: Vec<String> = match a.get("folders") {
                    Some(Value::Array(v)) => v.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
                    Some(Value::String(s)) => s.split(',').map(|f| f.trim().to_string()).filter(|f| !f.is_empty()).collect(),
                    _ => vec![],
                };
                let days = clamp(a.get("days"), 0, 3650, 0);
                let q = crate::mail::SearchQuery { query: string(a, "query"), sender: string(a, "sender"), recipient: string(a, "recipient") };
                let mut rows = m.search(&q, &folders, (days > 0).then_some(days), clamp(a.get("limit"), 1, 50, 15) as usize)
                    .map_err(mail_err)?;
                self.ctx.tag(&mut rows);
                Ok(json!({"count": rows.len(), "emails": self.shorten(&rows)?}))
            }
            "read_email" => {
                let eid = self.target(a, viewing)?;
                let msg = m.get_message(&eid).map_err(mail_err)?;
                self.processed(&eid);
                Ok(self.shorten(&[msg])?.remove(0))
            }
            "read_thread" => {
                let eid = self.target(a, viewing)?;
                let msgs = m.get_thread(&eid, crate::mail::THREAD_MAX, crate::mail::THREAD_CHARS).map_err(mail_err)?;
                self.processed(&eid);
                Ok(json!({"messages": self.shorten(&msgs)?}))
            }
            "find_related" => {
                let eid = self.target(a, viewing)?;
                let r = m.related(&eid).map_err(mail_err)?;
                let (sent, drafts): (Vec<_>, Vec<_>) = r.items.iter().partition(|i| i.folder == "sent");
                let sent: Vec<_> = sent.into_iter().cloned().collect();
                let drafts: Vec<_> = drafts.into_iter().cloned().collect();
                let summary = match (&r.last_action, sent.is_empty(), drafts.is_empty()) {
                    (Some(a), _, _) => format!("Outlook records that the user {a} this email{}.",
                                               r.last_action_at.as_ref().map(|t| format!(" on {t}")).unwrap_or_default()),
                    (None, false, _) => "The user sent related messages (see sent).".into(),
                    (None, true, false) => "Only unsent drafts exist (see drafts); nothing was sent.".into(),
                    (None, true, true) => "No reply, forward or draft found for this email.".into(),
                };
                Ok(json!({"summary": summary, "last_action": r.last_action, "last_action_at": r.last_action_at,
                          "sent": self.shorten(&sent)?, "drafts": self.shorten(&drafts)?}))
            }
            "find_followups" => {
                let days = clamp(a.get("days"), 1, 180, 10);
                crate::followup::refresh(m.as_ref(), &self.store, &self.ctx, days).map_err(mail_err)?;
                Ok(crate::followup::list(&self.store, days, |e| self.short(e).unwrap_or_default()))
            }
            "list_folders" => Ok(json!({"folders": m.list_folders(2).map_err(mail_err)?})),
            "create_reply_draft" => {
                let eid = self.target(a, viewing)?;
                let text = self.ctx.draft(&string(a, "body"), opt(a, "greeting_name"));
                let d = m.create_reply_draft(&eid, &text, boolean(a.get("reply_all"))).map_err(mail_err)?;
                self.processed(&eid);
                let sid = self.short(&d.entry_id)?;
                let _ = self.store.log_draft(conversation_id, "reply", &sid, &d.subject, &d.to);
                Ok(json!({"saved_to": "Drafts", "sent": false, "id": sid, "subject": d.subject, "to": d.to, "cc": d.cc}))
            }
            "create_forward_draft" | "create_new_draft" => {
                // Names → addresses from the user's correspondents first; ambiguous names are not guessed.
                let typed = crate::mail::split_recipients(&string(a, "to"));
                let (to, amb_to) = self.ctx.resolve_recipients(&string(a, "to"));
                let (cc, amb_cc) = self.ctx.resolve_recipients(&string(a, "cc"));
                // No greeting_name, one recipient typed in Arabic (e.g. «منى»): greet them as typed.
                let arabic_typed = (typed.len() == 1 && typed[0].chars().any(|c| ('\u{0600}'..='\u{06FF}').contains(&c)))
                    .then(|| typed[0].clone());
                let text = self.ctx.draft(&string(a, "body"), opt(a, "greeting_name").or(arabic_typed));
                let (d, kind) = if name == "create_forward_draft" {
                    let eid = self.target(a, viewing)?;
                    let d = m.create_forward_draft(&eid, &to, &text, &cc).map_err(mail_err)?;
                    self.processed(&eid);
                    (d, "forward")
                } else {
                    (m.create_new_draft(&to, &string(a, "subject"), &text, &cc).map_err(mail_err)?, "new")
                };
                let sid = self.short(&d.entry_id)?;
                let _ = self.store.log_draft(conversation_id, kind, &sid, &d.subject, &d.recipients.join("; "));
                let mut out = json!({"saved_to": "Drafts", "sent": false, "id": sid, "subject": d.subject,
                                     "recipients": d.recipients, "attachments": d.attachments});
                if !d.unresolved.is_empty() {
                    out["unresolved"] = json!(d.unresolved);
                    let cands: Map<String, Value> = amb_to.into_iter().chain(amb_cc)
                        .map(|(n, c)| (n, json!(c.into_iter().map(|c| format!("{} <{}>", c.name, c.email)).collect::<Vec<_>>())))
                        .collect();
                    if !cands.is_empty() {
                        out["candidates"] = Value::Object(cands);
                    }
                    out["note"] = json!("Not found or ambiguous; left in the draft as typed. Ask the user which person \
                                         they mean (see candidates), or they can click Check Names (Ctrl+K) in Outlook.");
                }
                Ok(out)
            }
            "find_recipient" => Ok(self.ctx.find_recipient(&string(a, "name"))),
            "list_drafts" => {
                let rows = m.list_drafts(clamp(a.get("limit"), 1, 30, 10) as usize).map_err(mail_err)?;
                Ok(json!({"drafts": self.shorten(&rows)?}))
            }
            _ => Err(format!("Unknown tool '{name}'.")),
        }
    }
}

/// Short human-readable label for the UI's activity line.
pub fn describe_call(name: &str, a: &Value) -> String {
    let s = |k: &str| a.get(k).map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string())).unwrap_or_default();
    match name {
        "list_emails" => {
            let mut bits = vec![Some(s("folder")).filter(|f| !f.is_empty()).unwrap_or_else(|| "inbox".into())];
            if boolean(a.get("unread_only")) { bits.push("unread".into()); }
            let d = s("days");
            if !d.is_empty() && d != "0" { bits.push(format!("last {d}d")); }
            format!("Listing {}", bits.join(", "))
        }
        "search_emails" => {
            let mut what = vec![];
            if !s("query").is_empty() { what.push(s("query")); }
            if !s("sender").is_empty() { what.push(format!("from {}", s("sender"))); }
            format!("Searching: {}", what.join(" / "))
        }
        "read_email" => format!("Reading email {}", s("id")),
        "read_thread" => format!("Reading thread {}", s("id")),
        "create_reply_draft" => format!("Saving reply draft to {}", s("id")),
        "create_forward_draft" => format!("Saving forward of {} to {}", s("id"), s("to")),
        "find_recipient" => format!("Looking up {}", s("name")),
        "find_related" => format!("Checking replies/forwards of {}", s("id")),
        "find_followups" => format!("Checking follow-ups (last {}d)", Some(s("days")).filter(|d| !d.is_empty()).unwrap_or_else(|| "10".into())),
        "create_new_draft" => format!("Saving new draft to {}", s("to")),
        other => {
            let t = other.replace('_', " ");
            let mut c = t.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_mail::FakeMail;

    fn setup() -> (Arc<FakeMail>, MailTools) {
        let fake = Arc::new(FakeMail::seeded());
        let tools = MailTools::new(fake.clone(), Arc::new(Store::in_memory()));
        (fake, tools)
    }

    fn call(t: &MailTools, name: &str, args: Value) -> Value {
        serde_json::from_str(&t.call(name, &args, Some("c1"))).unwrap()
    }

    #[test]
    fn list_search_read_and_draft() {
        let (fake, t) = setup();
        let r = call(&t, "list_emails", json!({"unread_only": true, "days": 7}));
        assert_eq!(r["count"], 2, "unread in last 7 days");
        assert!(r["emails"][0]["subject"].as_str().unwrap().starts_with("RE: Data lake"), "newest first");
        assert_eq!(r["emails"][0]["id"], "m1", "short ids assigned");
        assert!(r["emails"][0].get("entry_id").is_none(), "EntryID hidden from the model");

        let r = call(&t, "list_emails", json!({"days": "7"}));
        assert!(r["emails"].as_array().unwrap().iter().all(|e| !e["subject"].as_str().unwrap().contains("newsletter")),
                "date cutoff (string arg)");

        let r = call(&t, "search_emails", json!({"query": "الميزانية"}));
        assert_eq!(r["count"], 1);
        assert_eq!(r["emails"][0]["attachments"], 1, "Arabic keyword search");
        let r = call(&t, "search_emails", json!({"sender": "sara", "query": "cutover", "folders": ["inbox"]}));
        assert_eq!(r["count"], 2, "sender + keyword search");
        assert!(call(&t, "search_emails", json!({}))["error"].as_str().unwrap().contains("query"));

        let r = call(&t, "read_thread", json!({"id": "[m1]"}));
        assert!(!fake.state.lock().unwrap().iter().any(|m| m.subject.starts_with("RE: Data lake") && m.folder == "Inbox" && m.unread),
                "the email the assistant read is marked read");
        let msgs = r["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3, "thread with sent item");
        assert!(!msgs[1]["body"].as_str().unwrap().contains("old quoted stuff"), "quotes stripped");
        assert!(msgs[0]["received"].as_str() < msgs[2]["received"].as_str(), "oldest first");

        assert!(call(&t, "read_email", json!({"id": "m999"}))["error"].as_str().unwrap().contains("Unknown email id"));
        let r = call(&t, "list_emails", json!({"folder": "Inbox/Nope"}));
        let e = r["error"].as_str().unwrap();
        assert!(e.contains("not found") && e.contains("Projects"), "actionable folder error: {e}");

        let r = call(&t, "create_reply_draft", json!({"id": "m1", "body": "Hi Sara,\n\nConfirmed.\n\nAdel"}));
        assert_eq!(r["saved_to"], "Drafts");
        assert_eq!(r["sent"], false);
        let d = fake.drafts();
        assert!(d[0].html.find("Confirmed").unwrap() < d[0].html.find("From: Sara").unwrap(), "reply above quote");

        let r = call(&t, "create_new_draft", json!({"to": "k@example.com", "subject": "B", "body": "<script>x</script>"}));
        assert_eq!(r["saved_to"], "Drafts");
        let html = &fake.drafts()[1].html;
        assert!(!html.contains("<script>") && html.contains("&lt;script&gt;"), "draft HTML escaped");
        assert!(call(&t, "create_new_draft", json!({"to": " ", "subject": "x", "body": "y"}))["error"].is_string());
        assert_eq!(t.store.draft_log_count().unwrap(), 2, "drafts logged");
        assert_eq!(call(&t, "list_drafts", json!({}))["drafts"].as_array().unwrap().len(), 2);
        // "Did I reply to this?": Outlook's status + the reply in Sent Items; drafts count separately.
        let first = fake.state.lock().unwrap().iter().find(|m| m.subject == "Data lake migration - cutover date").unwrap().entry_id.clone();
        let sid = t.store.short_id(&first).unwrap();
        let r = call(&t, "find_related", json!({"id": sid}));
        assert_eq!(r["last_action"], "replied", "{r}");
        assert_eq!(r["sent"].as_array().unwrap().len(), 1);
        assert_eq!(r["sent"][0]["kind"], "reply");
        assert!(r["summary"].as_str().unwrap().contains("replied"));
        let r = call(&t, "find_related", json!({"id": "m1"})); // newest Sara email: only the reply drafted above
        assert!(r["sent"].as_array().unwrap().is_empty() && r["drafts"].as_array().unwrap().len() == 1, "{r}");
        assert!(r["summary"].as_str().unwrap().contains("nothing was sent"));
        // Recipient filter (in Sent Items the sender is always the user) and the Arabic hamza fallback.
        assert_eq!(call(&t, "search_emails", json!({"folders": ["sent"], "recipient": "Sara"}))["count"], 1);
        assert_eq!(call(&t, "search_emails", json!({"folders": ["sent"], "sender": "Sara"}))["count"], 0);
        assert_eq!(call(&t, "search_emails", json!({"query": "مواءمة اجتماع الميزانية"}))["count"], 1,
                   "exact phrase finds nothing → retried with its stable words");

        // Nothing learned yet: find_recipient falls back to a unique address-book match, never an ambiguous one.
        let r = call(&t, "find_recipient", json!({"name": "Mona"}));
        assert_eq!((r["matches"][0]["email"].as_str(), r["matches"][0]["source"].as_str()), (Some("mona@example.com"), Some("address book")));
        assert!(call(&t, "find_recipient", json!({"name": "Rania"}))["matches"].as_array().unwrap().is_empty());
        assert!(call(&t, "nope", json!({}))["error"].as_str().unwrap().contains("Unknown tool"));

        // forward: m2 is Khalid's Arabic email with one attachment
        let body = "مع التحية والتقدير للجميع\n\nنرجو إكمال هذه المهمة قبل يوم الثلاثاء.";
        let r = call(&t, "create_forward_draft", json!({"id": "m2", "to": "Rania; Mona، Hassan Alattar", "cc": "sara@example.com", "body": body}));
        assert_eq!(r["saved_to"], "Drafts");
        assert_eq!(r["sent"], false);
        assert_eq!(r["attachments"], 1, "forward keeps attachments");
        let recips: Vec<&str> = r["recipients"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert!(recips.contains(&"Mona Al-Khatib <mona@example.com>") && recips.contains(&"Hassan Alattar <hassan.attar@example.com>")
                && recips.contains(&"CC: Sara Al-Masri <sara@example.com>"), "resolved via address book: {recips:?}");
        assert_eq!(r["unresolved"], json!(["Rania"]), "ambiguous name reported, not guessed");
        assert!(r["note"].as_str().unwrap().contains("Check Names"));
        let fwd = fake.drafts().last().unwrap().clone();
        assert_eq!(fwd.subject, "FW: طلب اجتماع بخصوص ميزانية ٢٠٢٧");
        assert_eq!(fwd.to, "Rania; Mona Al-Khatib; Hassan Alattar", "unresolved name kept as typed");
        assert!(fwd.html.find("مع التحية").unwrap() < fwd.html.find("From: Khalid").unwrap(), "message above original");
        assert!(call(&t, "create_forward_draft", json!({"id": "m2", "to": " ; ", "body": "x"}))["error"].as_str().unwrap().contains("recipient"));
        let r = call(&t, "create_forward_draft", json!({"id": "m1", "to": "hassan.attar@example.com"}));
        assert!(r.get("unresolved").is_none());
        assert!(fake.drafts().last().unwrap().html.starts_with("<html><head></head><body lang=EN><hr>"), "no message → body untouched");
        assert_eq!(t.store.draft_log_count().unwrap(), 4, "forwards logged too");
        assert_eq!(describe_call("create_forward_draft", &json!({"id": "m2", "to": "Rania"})), "Saving forward of m2 to Rania");
    }

    #[test]
    fn style_recipients_and_tags() {
        let fake = Arc::new(FakeMail::demo());
        let store = Arc::new(Store::in_memory());
        let t = MailTools::new(fake.clone(), store.clone());
        t.ctx.save(crate::fake_mail::sample_profile()).unwrap();
        let p = t.ctx.profile();
        crate::learn::learn(fake.as_ref(), &store, &p.style, &p.learning, &Default::default()).unwrap();
        crate::learn::index_senders(fake.as_ref(), &store, 365, 500).unwrap();

        // relevance tags on lists (the sample mail is addressed to "Adel Ahmed" = whoami)
        let r = call(&t, "list_emails", json!({"days": 7}));
        assert!(r["emails"][0]["tags"].as_array().unwrap().contains(&json!("to_you")));

        // the user's writing style, applied by code
        let r = call(&t, "create_reply_draft", json!({"id": "m1", "body": "تمت الموافقة.", "greeting_name": "الأستاذة سارة"}));
        assert_eq!(r["saved_to"], "Drafts");
        let html = fake.drafts().last().unwrap().html.clone();
        assert!(html.contains("مع التحية والتقدير للأستاذة سارة") && html.contains("مع تحياتي<br>-عادل"), "{html}");
        assert!(html.contains("Sakkal Majalla") && html.contains(r#"dir="rtl""#));
        call(&t, "create_reply_draft", json!({"id": "m1", "body": "شكراً", "reply_all": true}));
        assert!(fake.drafts().last().unwrap().html.contains("مع التحية والتقدير للجميع"), "reply-all greets everyone");

        // recipients: names → addresses from correspondents; ambiguous names are not guessed
        let r = call(&t, "create_new_draft", json!({"to": "Mona", "subject": "x", "body": "مرحبا"}));
        assert_eq!(r["recipients"], json!(["Mona Al-Khatib <mona@example.com>"]), "{r}");
        assert!(r.get("unresolved").is_none());
        let r = call(&t, "create_new_draft", json!({"to": "Rania", "subject": "x", "body": "مرحبا"}));
        assert_eq!(r["unresolved"], json!(["Rania"]));
        assert_eq!(r["candidates"]["Rania"].as_array().unwrap().len(), 2, "{r}");
        let r = call(&t, "create_forward_draft", json!({"id": "m2", "to": "mona", "body": "للاطلاع"}));
        assert_eq!(r["recipients"], json!(["Mona Al-Khatib <mona@example.com>"]));
        let r = call(&t, "find_recipient", json!({"name": "Mona"}));
        assert_eq!(r["matches"][0]["email"], "mona@example.com");

        // A meeting invitation is forwarded as an email with the invitation attached.
        let inv = fake.state.lock().unwrap().iter().find(|m| m.class == 53).unwrap().entry_id.clone();
        let inv_id = store.short_id(&inv).unwrap();
        let r = call(&t, "create_forward_draft", json!({"id": inv_id, "to": "ceo@example.com", "body": "نأمل دعمكم.", "greeting_name": "سعادة الرئيس التنفيذي"}));
        assert_eq!(r["attachments"], 1, "{r}");
        let d = fake.drafts().last().unwrap().clone();
        assert!(d.subject.starts_with("FW: ورشة تخطيط المشاريع") && d.class == 43, "a normal email, not a meeting forward");
        assert_eq!(d.attachments, ["ورشة تخطيط المشاريع — المستندات الداعمة.msg"]);
        assert!(d.html.find("نأمل دعمكم").unwrap() < d.html.find("الدعوة المرفقة").unwrap(), "message above the invitation details");

        let r = call(&t, "find_recipient", json!({"name": "منى"}));
        assert_eq!(r["matches"][0]["email"], "mona@example.com", "Arabic spelling finds the Latin-named contact");
        let r = call(&t, "create_new_draft", json!({"to": "منى", "subject": "ورشة", "body": "نؤكد المشاركة."}));
        assert_eq!(r["recipients"], json!(["Mona Al-Khatib <mona@example.com>"]), "{r}");
        assert!(fake.drafts().last().unwrap().html.contains("مع التحية والتقدير لمنى"), "greeted as typed in Arabic");
        let r = call(&t, "find_recipient", json!({"name": "زينب"}));
        assert!(r["matches"].as_array().unwrap().is_empty() && !r["frequent_contacts"].as_array().unwrap().is_empty(),
                "unknown names fall back to frequent contacts for the model to match");
        assert!(r["frequent_contacts"].as_array().unwrap().iter().all(|c| c["email"] != "adel@example.com"), "self excluded");
    }

    #[test]
    fn labels() {
        assert_eq!(describe_call("list_emails", &json!({"unread_only": true, "days": 7})), "Listing inbox, unread, last 7d");
        assert_eq!(describe_call("list_folders", &json!({})), "List folders");
    }
}
