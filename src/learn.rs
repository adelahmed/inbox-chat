//! Learning the user's reply behaviour from Sent Items.
//!
//! Each sent reply carries the email it answered as quoted text below it. We split it into
//! (who wrote in, what they wrote) → (what the user replied), store it locally, and later pick the
//! most similar past replies as examples for new drafts and suggestions. Read-only on Outlook.

use crate::mail::{MailResult, MailStore, SentItem};
use crate::profile::{name_matches, norm, Learning, Style};
use crate::store::{Example, Store};
use crate::text::{clean_text, quote_start, strip_quoted, truncate_chars};
use serde::Serialize;
use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};

const FROM_KEYS: &[&str] = &["from", "من"];
const OTHER_HEADER_KEYS: &[&str] = &["sent", "date", "to", "cc", "subject", "تاريخ الارسال", "ارسل", "التاريخ", "الي",
                                     "نسخه", "نسخه الي", "الموضوع", "importance", "الاهميه"];

/// Split a sent reply into an example. None if it isn't a reply to someone's email.
pub fn parse_sent(item: &SentItem, style: &Style) -> Option<Example> {
    let body = clean_text(&item.body);
    let start = quote_start(&body)?;
    let (mine, quoted) = body.split_at(start);

    // The user's own text: cut at the sign-off (an Outlook signature may follow), drop the greeting.
    let sign_first = style.signoff.lines().next().map(norm).unwrap_or_default();
    let mut lines: Vec<&str> = mine.lines().collect();
    if let Some(i) = lines.iter().position(|l| !sign_first.is_empty() && norm(l) == sign_first) {
        lines.truncate(i);
    }
    let reply = crate::profile::strip_own_greeting_signoff(style, &lines.join("\n"));
    if reply.trim().is_empty() {
        return None;
    }

    // The quoted email: header lines (From/Sent/To/Subject, English or Arabic), then its body.
    let mut from = String::new();
    let mut rest = Vec::new();
    let mut in_header = true;
    for line in quoted.lines() {
        let t = line.trim();
        if in_header {
            if t.is_empty() || t.chars().all(|c| c == '_' || c == '-') || norm(t).contains("original message") {
                if !from.is_empty() && t.is_empty() {
                    in_header = false;
                }
                continue;
            }
            if let Some((k, v)) = t.split_once(':') {
                let key = norm(k);
                if FROM_KEYS.contains(&key.as_str()) {
                    from = v.trim().to_string();
                    continue;
                }
                if OTHER_HEADER_KEYS.contains(&key.as_str()) {
                    continue;
                }
            }
            in_header = false;
        }
        rest.push(line);
    }
    let incoming = truncate_chars(&strip_quoted(&rest.join("\n")), 1500);
    if from.is_empty() && incoming.is_empty() {
        return None;
    }
    let subject = item.subject.trim();
    Some(Example {
        sent_id: item.entry_id.clone(),
        sent_at: item.sent_at.clone(),
        recipients: item.to.clone(),
        subject: subject.to_string(),
        incoming_from: from,
        incoming_text: incoming,
        reply_text: truncate_chars(&reply, 1500),
    })
}

const STOP: &[&str] = &["the", "and", "for", "you", "with", "this", "that", "are", "from", "have", "please", "regards",
                        "على", "الى", "الي", "في", "من", "عن", "مع", "هذا", "هذه", "التي", "الذي", "تم", "ان", "او", "كما"];

fn tokens(s: &str) -> HashSet<String> {
    norm(s).split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() >= 3 && !STOP.contains(w))
        .map(String::from).collect()
}

/// The `k` past replies most like this email: same sender first, then shared words.
pub fn similar(examples: &[Example], from: &str, subject: &str, text: &str, k: usize) -> Vec<Example> {
    let q = tokens(&format!("{subject} {}", truncate_chars(text, 1500)));
    let mut scored: Vec<(f64, &Example)> = examples.iter().map(|e| {
        let t = tokens(&format!("{} {}", e.subject, e.incoming_text));
        let inter = q.intersection(&t).count() as f64;
        let union = (q.len() + t.len()) as f64 - inter;
        let jacc = if union > 0.0 { inter / union } else { 0.0 };
        let same = !from.trim().is_empty() && !e.incoming_from.is_empty()
            && (name_matches(&e.incoming_from, from) || name_matches(from, &e.incoming_from));
        (jacc + if same { 1.0 } else { 0.0 }, e)
    }).collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut out: Vec<Example> = scored.iter().filter(|(s, _)| *s > 0.04).take(k).map(|(_, e)| (*e).clone()).collect();
    for e in examples {
        if out.len() >= k {
            break;
        }
        if !out.iter().any(|o| o.sent_id == e.sent_id) {
            out.push(e.clone()); // top up with the most recent
        }
    }
    out
}

pub fn median_words(examples: &[Example]) -> usize {
    let mut w: Vec<usize> = examples.iter().map(|e| e.reply_text.split_whitespace().count()).collect();
    w.sort_unstable();
    w.get(w.len() / 2).copied().unwrap_or(0)
}

/// Examples formatted for a prompt.
pub fn format_examples(examples: &[Example]) -> String {
    examples.iter().enumerate().map(|(i, e)| format!(
        "Example {}:\nIncoming from {} — subject «{}»:\n{}\nThe user's reply:\n{}",
        i + 1, if e.incoming_from.is_empty() { "?" } else { &e.incoming_from }, e.subject,
        truncate_chars(&e.incoming_text, 500), truncate_chars(&e.reply_text, 500),
    )).collect::<Vec<_>>().join("\n\n")
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct LearnReport {
    pub scanned: usize,
    pub added: usize,
    pub total: i64,
}

/// Scan Sent Items (newest first) and store new reply examples. Works in small batches so other
/// Outlook requests aren't blocked for long. Stops at items already seen, the age limit or the cap.
pub fn learn(mail: &dyn MailStore, store: &Store, style: &Style, cfg: &Learning, progress: &AtomicUsize) -> MailResult<LearnReport> {
    let cutoff = (chrono::Local::now().naive_local() - chrono::Duration::days(cfg.days_back)).format("%Y-%m-%dT%H:%M").to_string();
    let db = |e: rusqlite::Error| crate::mail::MailError::Outlook(format!("Storage error: {e}"));
    let mut report = LearnReport::default();
    let (mut offset, mut seen_streak) = (0usize, 0usize);
    'outer: loop {
        let (batch, more) = mail.sent_items(offset, 25)?;
        for it in &batch {
            if it.sent_at.as_deref().is_some_and(|d| d < cutoff.as_str()) {
                break 'outer;
            }
            report.scanned += 1;
            progress.store(report.scanned, Ordering::Relaxed);
            for (name, email) in &it.recipients {
                store.add_contact(&it.entry_id, name, email, it.sent_at.as_deref()).map_err(db)?;
            }
            if store.is_seen(&it.entry_id).map_err(db)? {
                seen_streak += 1;
                if seen_streak >= 40 {
                    break 'outer; // the rest was learned in an earlier run
                }
                continue;
            }
            seen_streak = 0;
            if let Some(ex) = parse_sent(it, style) {
                store.add_example(&ex).map_err(db)?;
                report.added += 1;
            }
            store.mark_seen(&it.entry_id).map_err(db)?;
            if store.example_count().map_err(db)? as usize >= cfg.max_examples {
                break 'outer;
            }
        }
        offset += batch.len();
        if !more || batch.is_empty() {
            break;
        }
    }
    report.total = store.example_count().map_err(db)?;
    Ok(report)
}

/// Index Inbox senders as correspondents (newest first; stops at already-indexed items or the age limit).
pub fn index_senders(mail: &dyn MailStore, store: &Store, days_back: i64, max_items: usize) -> MailResult<usize> {
    let cutoff = (chrono::Local::now().naive_local() - chrono::Duration::days(days_back)).format("%Y-%m-%dT%H:%M").to_string();
    let db = |e: rusqlite::Error| crate::mail::MailError::Outlook(format!("Storage error: {e}"));
    let (mut offset, mut added, mut known_streak) = (0usize, 0usize, 0usize);
    loop {
        let (batch, more) = mail.recent_senders(offset, 50)?;
        for s in &batch {
            if s.when.as_deref().is_some_and(|d| d < cutoff.as_str()) || offset >= max_items {
                return Ok(added);
            }
            if store.add_contact(&s.item_id, &s.name, &s.email, s.when.as_deref()).map_err(db)? {
                added += 1;
                known_streak = 0;
            } else {
                known_streak += 1;
                if known_streak >= 60 {
                    return Ok(added);
                }
            }
        }
        offset += batch.len();
        if !more || batch.is_empty() {
            return Ok(added);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_mail::FakeMail;

    fn sent(body: &str) -> SentItem {
        SentItem { entry_id: "S1".into(), sent_at: Some("2026-11-01T10:00".into()), to: "Khalid Mansour".into(),
                   subject: "RE: طلب بيانات".into(), body: body.into(), recipients: vec![] }
    }

    #[test]
    fn parses_english_and_arabic_quote_headers() {
        let st = crate::fake_mail::sample_profile().style;
        let e = parse_sent(&sent("مع التحية والتقدير لخالد\n\nتم، سنرسلها الأحد.\n\nمع تحياتي\n-عادل\nمدير عام\n\nFrom: Khalid Mansour\nSent: Sunday\nTo: Adel\nSubject: طلب\n\nنرجو تزويدنا بالبيانات.\n\nFrom: X\nSent: y\nold"), &st).unwrap();
        assert_eq!(e.reply_text, "تم، سنرسلها الأحد.", "greeting, sign-off and signature removed");
        assert_eq!(e.incoming_from, "Khalid Mansour");
        assert_eq!(e.incoming_text, "نرجو تزويدنا بالبيانات.", "nested quote removed");
        let e = parse_sent(&sent("موافق.\n\nمن: سارة المصري\nتاريخ الإرسال: الأحد\nإلى: عادل\nالموضوع: اعتماد\n\nنأمل الاعتماد."), &st).unwrap();
        assert_eq!((e.incoming_from.as_str(), e.incoming_text.as_str(), e.reply_text.as_str()), ("سارة المصري", "نأمل الاعتماد.", "موافق."));
        assert!(parse_sent(&sent("A new email, not a reply."), &st).is_none());
    }

    #[test]
    fn learns_incrementally_and_finds_similar() {
        let fake = FakeMail::demo();
        let store = Store::in_memory();
        let (st, cfg, p) = (crate::fake_mail::sample_profile().style, Learning::default(), AtomicUsize::new(0));
        let r = learn(&fake, &store, &st, &cfg, &p).unwrap();
        assert!(r.added >= 12, "demo replies learned: {r:?}");
        let again = learn(&fake, &store, &st, &cfg, &p).unwrap();
        assert_eq!(again.added, 0, "nothing re-learned");
        let ex = store.examples(100).unwrap();
        let top = similar(&ex, "Khalid Mansour", "طلب بيانات", "نرجو تزويدنا بالبيانات", 3);
        assert_eq!(top.len(), 3);
        assert!(top.iter().all(|e| e.incoming_from == "Khalid Mansour"), "same sender first");
        assert!(median_words(&ex) > 0);
        assert!(format_examples(&top[..1]).contains("The user's reply:"));
    }
}
