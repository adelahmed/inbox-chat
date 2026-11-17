//! Follow-ups: requests the user sent that are still waiting for an answer.
//!
//! 1. New Sent Items (within the window) are read once and stored: is it a request? to whom? what was asked?
//! 2. Requests still open are re-checked: did someone on the To line write back in the same conversation
//!    after the request? Then it's `followup:closed`, otherwise `followup:open`.
//! Closed and dismissed items are never re-checked, so repeat runs are cheap. Read-only on Outlook.

use crate::context::UserContext;
use crate::mail::{MailResult, MailStore, SentItem};
use crate::profile::{name_matches, norm};
use crate::store::{FollowupRow, Store};
use crate::text::{clean_text, strip_quoted, truncate_chars};
use serde_json::{json, Value};

/// Request verbs, matched as whole words (a leading و/ف is allowed: «وآمل», «فنرجو»). Nouns like
/// مراجعة/متابعة/إعداد are NOT signals on their own: «تمت مراجعة…» is a report, «نأمل مراجعة…» a request.
const ASK_WORDS: &[&str] = &["نأمل", "نرجو", "يرجى", "أرجو", "الرجاء", "آمل", "نود", "التكرم", "تكرمكم", "please", "kindly", "pls"];
/// Phrases that are requests by themselves.
const ASK_PHRASES: &[&str] = &[
    "أمل منكم", "إفادتنا", "إفادتي", "موافاتنا", "موافاتي", "تزويدنا", "تزويدي", "الرد على الجهة", "الرد عليهم", "اطلاعي على",
    "could you", "can you", "would you", "let me know", "get back to", "send me", "share with me", "your feedback",
    "your input", "your comments", "by end of", "asap",
];

static URL: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"(?i)\b(?:https?://|www\.)\S+").unwrap());

/// Which rules make a text a request (also used for diagnostics: rule names only, no content).
pub fn request_reasons(text: &str) -> Vec<String> {
    let t = norm(&URL.replace_all(text, " ")); // a '?' inside a link is not a question
    let words: Vec<&str> = t.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let mut r: Vec<String> = ASK_WORDS.iter().filter(|a| {
        let a = norm(a);
        words.iter().any(|w| *w == a || w.strip_prefix(['و', 'ف']).is_some_and(|rest| rest == a))
    }).map(|a| a.to_string()).collect();
    r.extend(ASK_PHRASES.iter().filter(|p| t.contains(&norm(p))).map(|p| p.to_string()));
    if t.contains(['?', '؟']) {
        r.push("question mark".into());
    }
    r
}

/// Is this text (the user's own part of a sent email) asking someone to do something?
pub fn is_request(subject: &str, text: &str) -> bool {
    let t = norm(text);
    if t.chars().count() < 12 && !t.contains(['?', '؟']) {
        return false; // "thanks", "noted", "تم" …
    }
    let s = norm(subject);
    if ["accepted:", "declined:", "tentative:", "مقبول:", "مرفوض:", "مبدئي:"].iter().any(|p| s.starts_with(&norm(p))) {
        return false; // meeting responses
    }
    !request_reasons(text).is_empty()
}

/// The user's own words in a sent email: quoted original removed, greeting and sign-off dropped.
pub fn own_text(body: &str, ctx: &UserContext) -> String {
    let mine = strip_quoted(&clean_text(body));
    let style = ctx.profile().style;
    let sign = style.signoff.lines().next().map(norm).unwrap_or_default();
    let mut lines: Vec<&str> = mine.lines().collect();
    if let Some(i) = lines.iter().position(|l| !sign.is_empty() && norm(l) == sign) {
        lines.truncate(i); // anything after the sign-off is the Outlook signature
    }
    crate::profile::strip_own_greeting_signoff(&style, &lines.join("\n"))
}

/// To recipients of a sent item: (display name, address), excluding the user.
fn to_recipients(it: &SentItem, me: &[String]) -> Vec<(String, String)> {
    let to_names: Vec<String> = it.to.split(';').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    let is_me = |n: &str, e: &str| me.iter().any(|m| m.eq_ignore_ascii_case(e) || name_matches(n, m) || name_matches(m, n));
    let mut out: Vec<(String, String)> = it.recipients.iter()
        .filter(|(n, _)| to_names.iter().any(|t| t == n))
        .filter(|(n, e)| !is_me(n, e))
        .cloned().collect();
    if out.is_empty() {
        // Recipients collection unavailable: fall back to the To line's names.
        out = to_names.into_iter().filter(|n| !is_me(n, "")).map(|n| (n, String::new())).collect();
    }
    out
}

fn same_person(a: (&str, &str), b: (&str, &str)) -> bool {
    (!a.1.is_empty() && a.1.eq_ignore_ascii_case(b.1)) || (!a.0.is_empty() && !b.0.is_empty() && (name_matches(a.0, b.0) || name_matches(b.0, a.0)))
}

/// Read new Sent Items into the store, then re-check open requests. Returns how many were (re)checked.
pub fn refresh(mail: &dyn MailStore, store: &Store, ctx: &UserContext, days: i64) -> MailResult<usize> {
    let db = |e: rusqlite::Error| crate::mail::MailError::Outlook(format!("Storage error: {e}"));
    let since = (chrono::Local::now().naive_local() - chrono::Duration::days(days)).format("%Y-%m-%dT%H:%M").to_string();
    let me = ctx.me();

    // 1. New sent emails (newest first). Stop at the window edge or after a run of known items.
    let (mut offset, mut known_streak) = (0usize, 0usize);
    'scan: loop {
        let (batch, more) = mail.sent_items(offset, 25)?;
        for it in &batch {
            if it.sent_at.as_deref().is_some_and(|d| d < since.as_str()) {
                break 'scan;
            }
            if store.followup_known(&it.entry_id).map_err(db)? {
                known_streak += 1;
                if known_streak >= 30 {
                    break 'scan;
                }
                continue;
            }
            known_streak = 0;
            let text = own_text(&it.body, ctx);
            let recipients = to_recipients(it, &me);
            store.save_followup(&FollowupRow {
                entry_id: it.entry_id.clone(),
                sent_at: it.sent_at.clone(),
                subject: it.subject.clone(),
                is_request: !recipients.is_empty() && is_request(&it.subject, &text),
                asked: truncate_chars(&text, 400),
                recipients,
                status: "open".into(),
                ..Default::default()
            }).map_err(db)?;
        }
        offset += batch.len();
        if !more || batch.is_empty() || offset >= 600 {
            break;
        }
    }

    // 2. Re-check open requests: a later message from a To recipient closes it.
    let mut checked = 0;
    for mut row in store.followups_since(&since).map_err(db)?.into_iter().filter(|r| r.status == "open").take(60) {
        let later = mail.later_in_conversation(&row.entry_id).unwrap_or_default();
        let sent_at = row.sent_at.clone();
        let after = |w: &Option<String>| w.as_deref().zip(sent_at.as_deref()).is_none_or(|(w, s)| w > s);
        let mine = |e: &crate::mail::ThreadEvent| me.iter().any(|m| same_person((&e.from_name, &e.from), (m, m)));
        row.others_replied.clear();
        for e in later.iter().filter(|e| after(&e.when)) {
            if mine(e) {
                row.you_followed_up_at = e.when.clone();
            } else if row.recipients.iter().any(|(n, a)| same_person((n, a), (&e.from_name, &e.from))) {
                row.status = "closed".into();
                row.closed_by = Some(e.from_name.clone());
                row.closed_at = e.when.clone();
                row.reply_entry_id = Some(e.entry_id.clone());
                break;
            } else if !row.others_replied.contains(&e.from_name) {
                row.others_replied.push(e.from_name.clone());
            }
        }
        store.save_followup(&row).map_err(db)?;
        checked += 1;
    }
    Ok(checked)
}

fn days_since(iso: Option<&str>) -> i64 {
    iso.and_then(|s| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M").ok())
        .map(|d| (chrono::Local::now().naive_local() - d).num_days()).unwrap_or(0)
}

/// The follow-up list for the model and the UI: only requests still waiting for an answer (answered
/// ones are left out — nothing to do). `shorten` maps EntryIDs to short ids (m12).
pub fn list(store: &Store, days: i64, shorten: impl Fn(&str) -> String) -> Value {
    let since = (chrono::Local::now().naive_local() - chrono::Duration::days(days)).format("%Y-%m-%dT%H:%M").to_string();
    let rows = store.followups_since(&since).unwrap_or_default();
    let open: Vec<Value> = rows.iter().filter(|r| r.status != "closed").take(25).map(|r| {
        let mut v = json!({
            "id": shorten(&r.entry_id),
            "to": r.recipients.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>(),
            "sent_at": r.sent_at, "days_waiting": days_since(r.sent_at.as_deref()),
            "subject": r.subject, "asked": truncate_chars(&r.asked, 180),
        });
        if let Some(t) = &r.you_followed_up_at {
            v["you_chased_on"] = json!(t);
        }
        if !r.others_replied.is_empty() {
            v["others_replied"] = json!(r.others_replied);
        }
        v
    }).collect();
    json!({
        "window_days": days,
        "count": open.len(),
        "dismissed_count": store.dismissed_followup_count().unwrap_or(0),
        "open": open,
        "note": "Requests the user sent that the person asked hasn't answered yet (answered ones are left out). \
                 The app shows this list with Open/Dismiss buttons under your answer.",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_mail::FakeMail;
    use std::sync::Arc;

    #[test]
    fn detects_requests() {
        assert!(is_request("طلب", "نأمل دراسة العرض والرد على الجهة قبل الخميس."));
        assert!(is_request("Data", "Could you send me the figures by Sunday?"));
        assert!(is_request("x", "وآمل منكم التكرم بالاطلاع والإفادة."), "و + verb");
        assert!(!is_request("RE: x", "شكراً لكم."));
        assert!(!is_request("تقرير", "تمت مراجعة التقرير بشكل كامل وشامل ضمن إطار التكامل."), "nouns / كامل / شامل aren't asks");
        assert!(!is_request("link", "The dashboard is at https://example.org/q?id=3 for reference."), "'?' in a link");
        assert!(!is_request("Accepted: Meeting", "I will attend, see you there please"));
        assert!(!is_request("FYI", "For your information, the report is attached."));
    }

    #[test]
    fn open_closed_and_dismissed() {
        let fake = Arc::new(FakeMail::demo());
        let store = Arc::new(Store::in_memory());
        let ctx = UserContext::load(store.clone(), fake.clone());
        ctx.save(crate::fake_mail::sample_profile()).unwrap(); // the demo's sent emails use its sign-off
        refresh(fake.as_ref(), &store, &ctx, 10).unwrap();
        let v = list(&store, 10, |e| e[e.len() - 4..].to_string());
        let open: Vec<&Value> = v["open"].as_array().unwrap().iter().collect();
        let mona = open.iter().find(|i| i["to"][0] == "Mona Al-Khatib").expect("open request to Mona");
        assert_eq!(mona["days_waiting"], 3);
        assert!(mona.get("status").is_none(), "no tags");
        assert!(mona["asked"].as_str().unwrap().contains("دراسة العرض") && !mona["asked"].as_str().unwrap().contains("مع تحياتي"));
        assert!(!open.iter().any(|i| i["to"][0] == "Hassan Alattar"), "answered requests aren't listed");
        let rows = store.followups_since("2000").unwrap();
        let hassan = rows.iter().find(|r| r.recipients[0].0 == "Hassan Alattar").expect("Hassan's request is tracked");
        assert_eq!((hassan.status.as_str(), hassan.closed_by.as_deref()), ("closed", Some("Hassan Alattar")), "closed by his reply");
        assert!(!open.iter().any(|i| i["subject"].as_str().unwrap().contains("شكر")), "thank-you notes aren't requests");
        assert!(!open.iter().any(|i| i["to"][0] == "Sara Al-Masri"), "19-day-old request is outside the 10-day window");

        // Dismissed items disappear from later queries; a second run re-reads nothing new.
        let mona_eid = rows.iter().find(|r| r.recipients[0].0 == "Mona Al-Khatib").unwrap().entry_id.clone();
        assert!(store.set_followup_dismissed(&mona_eid, true).unwrap());
        refresh(fake.as_ref(), &store, &ctx, 10).unwrap();
        let v = list(&store, 10, |e| e.to_string());
        assert!(!v["open"].as_array().unwrap().iter().any(|i| i["id"] == json!(mona_eid)));
        assert_eq!(v["dismissed_count"], 1);
        assert_eq!(store.restore_all_followups().unwrap(), 1);
    }
}
