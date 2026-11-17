//! Read-only check of the real Outlook connection. Prints counts and yes/no answers only — never mail
//! content — and never writes anything:
//!     cargo run --bin outlook-selftest

#[cfg(windows)]
fn main() {
    use inbox_chat::mail::MailStore;
    use inbox_chat::outlook::OutlookMail;

    let o = OutlookMail::start();
    let mut failures = 0;

    // --forward-dryrun mN: write a styled message into an in-memory forward of email mN (from the app's
    // id registry) and discard it. Nothing is saved or sent. Prints only how it was written.
    let args: Vec<String> = std::env::args().collect();
    // --followups-diag: which request rules fire on the last 30 days of Sent Items (rule names, counts and
    // text lengths only).
    if args.iter().any(|a| a == "--followups-diag") {
        use std::sync::Arc;
        let store = Arc::new(inbox_chat::store::Store::memory().unwrap());
        let mail: Arc<dyn MailStore> = Arc::new(o);
        let ctx = inbox_chat::context::UserContext::load(store.clone(), mail.clone());
        let since = (chrono::Local::now().naive_local() - chrono::Duration::days(30)).format("%Y-%m-%dT%H:%M").to_string();
        let (mut counts, mut n, mut own_len, mut body_len, mut quoted) = (std::collections::BTreeMap::<String, usize>::new(), 0, 0usize, 0usize, 0);
        let (mut off, mut stop) = (0, false);
        while !stop {
            let (batch, more) = mail.sent_items(off, 25).unwrap();
            for it in &batch {
                if it.sent_at.as_deref().is_some_and(|d| d < since.as_str()) { stop = true; break; }
                let own = inbox_chat::followup::own_text(&it.body, &ctx);
                n += 1; own_len += own.chars().count(); body_len += it.body.chars().count();
                if inbox_chat::text::quote_start(&inbox_chat::text::clean_text(&it.body)).is_some() { quoted += 1; }
                for r in inbox_chat::followup::request_reasons(&own) { *counts.entry(r).or_default() += 1; }
            }
            off += batch.len();
            if !more || batch.is_empty() { break; }
        }
        println!("PASS {n} sent emails; quoted original detected in {quoted}; avg body {} chars, avg own text {} chars",
                 body_len / n.max(1), own_len / n.max(1));
        let mut v: Vec<_> = counts.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        println!("PASS rule hits: {v:?}");
        return;
    }
    // --followups: run the follow-up scan into an in-memory store (twice, to time the cached run).
    // Read-only on Outlook; prints counts and timings only.
    if args.iter().any(|a| a == "--followups") {
        use std::sync::Arc;
        let store = Arc::new(inbox_chat::store::Store::memory().unwrap());
        let mail: Arc<dyn MailStore> = Arc::new(o);
        let ctx = inbox_chat::context::UserContext::load(store.clone(), mail.clone());
        for run in ["first run", "second run (cached)"] {
            let t = std::time::Instant::now();
            match inbox_chat::followup::refresh(mail.as_ref(), &store, &ctx, 10) {
                Ok(checked) => {
                    let v = inbox_chat::followup::list(&store, 10, |e| e[..8].to_string());
                    println!("PASS follow-ups {run} (10 days): {} waiting, {checked} re-checked, {:.1}s", v["count"], t.elapsed().as_secs_f64());
                }
                Err(e) => println!("FAIL follow-ups {run}: {e}"),
            }
        }
        let all = store.followups_since("2000").unwrap_or_default();
        println!("PASS sent emails read: {} (of which requests: {})", all.len(), all.iter().filter(|r| r.is_request).count());
        return;
    }
    // --bodies [N]: what the reading pane gets for the newest N Inbox items (default 150): Outlook body
    // format, whether HTML came back, tables/styles present, sizes and read time. Counts only.
    if let Some(pos) = args.iter().position(|a| a == "--bodies") {
        let n: usize = args.get(pos + 1).and_then(|v| v.parse().ok()).unwrap_or(150);
        let (mut items, mut off) = (Vec::new(), 0);
        while items.len() < n {
            let (batch, more) = o.page("inbox", off, 50).expect("page inbox");
            off += batch.len();
            items.extend(batch);
            if !more || off == 0 { break; }
        }
        items.truncate(n);
        let mut fmt = std::collections::BTreeMap::<String, usize>::new();
        let (mut empty_html, mut tables, mut styles, mut style_attr, mut slowest, mut errors) = (0, 0, 0, 0, 0f64, 0);
        let (mut rtf_formatted, mut rtf) = (0, 0);
        let mut meetings: Vec<(f64, bool, bool, bool)> = Vec::new();
        let mut notes: Vec<f64> = Vec::new();
        for s in &items {
            let t = std::time::Instant::now();
            let kind = match o.body_format(&s.entry_id) {
                Ok((f, class, extra)) => format!("{} {} {extra}", match f { 1 => "plain", 2 => "html", 3 => "rich-text", _ => "unknown" },
                                          class.split('.').take(2).collect::<Vec<_>>().join(".")),
                Err(_) => "error".into(),
            };
            *fmt.entry(kind.clone()).or_default() += 1;
            match o.read_full(&s.entry_id) {
                Ok(m) => {
                    let h = m.html.to_ascii_lowercase();
                    if h.trim().is_empty() { empty_html += 1; *fmt.entry(format!("EMPTY-HTML {kind}")).or_default() += 1; }
                    if h.contains("<table") { tables += 1; }
                    if h.contains("<style") { styles += 1; }
                    if h.contains("style=") { style_attr += 1; }
                    if kind.contains("IPM.Schedule") || kind.contains("IPM.Note") {
                        // Recovered from RTFBody: compare with Outlook's own plain text (word overlap, no content shown).
                        let shown = inbox_chat::text::html_to_text(&m.html);
                        let words: Vec<&str> = m.text.split_whitespace().filter(|w| w.chars().count() >= 3).collect();
                        let hit = words.iter().filter(|w| shown.contains(*w)).count();
                        let ratio = if words.is_empty() { 1.0 } else { hit as f64 / words.len() as f64 };
                        if kind.contains("IPM.Note") { notes.push(ratio); } else { meetings.push((ratio, h.contains("<table"), m.html.contains('\u{FFFD}'), m.html.chars().any(|c| ('\u{0600}'..='\u{06FF}').contains(&c)))); }
                    }
                    if kind.starts_with("rich-text") {
                        rtf += 1;
                        if h.contains("<b>") || h.contains("<font") || h.contains("style=") || h.contains("<table") { rtf_formatted += 1; }
                    }
                }
                Err(_) => errors += 1,
            }
            slowest = slowest.max(t.elapsed().as_secs_f64());
        }
        println!("PASS read {} inbox items: formats {fmt:?}", items.len());
        println!("PASS html empty in {empty_html}; <table> in {tables}; <style> in {styles}; style= in {style_attr}; read errors {errors}");
        println!("PASS rich-text items {rtf}, of which HTML keeps formatting: {rtf_formatted}; slowest read {slowest:.1}s");
        if !notes.is_empty() {
            println!("PASS baseline, ordinary HTML emails {}: body words found avg {:.0}%, min {:.0}%", notes.len(),
                     notes.iter().sum::<f64>() / notes.len() as f64 * 100.0, notes.iter().cloned().fold(1.0, f64::min) * 100.0);
        }
        if !meetings.is_empty() {
            let min = meetings.iter().map(|m| m.0).fold(1.0, f64::min);
            let avg = meetings.iter().map(|m| m.0).sum::<f64>() / meetings.len() as f64;
            println!("PASS meeting items {}: body words found in the shown HTML avg {:.0}%, min {:.0}%; with tables {}; with U+FFFD {}; with Arabic {}",
                     meetings.len(), avg * 100.0, min * 100.0, meetings.iter().filter(|m| m.1).count(),
                     meetings.iter().filter(|m| m.2).count(), meetings.iter().filter(|m| m.3).count());
        }
        return;
    }
    // --related mN "phrase": what the user did with email mN, plus the phrase search in Sent Items
    // (exact and with the word fallback). Read-only; prints counts and kinds only.
    if let Some(pos) = args.iter().position(|a| a == "--related") {
        let short = args.get(pos + 1).cloned().unwrap_or_default();
        let store = inbox_chat::store::Store::open(&inbox_chat::store::Store::default_path()).expect("open app database");
        let eid = store.entry_id(&short).ok().flatten().expect("known id");
        match o.related(&eid) {
            Ok(r) => println!("PASS related({short}): last_action={:?} at_set={} | sent: {:?} | drafts: {}",
                              r.last_action, r.last_action_at.is_some(),
                              r.items.iter().filter(|i| i.folder == "sent").map(|i| i.kind.clone()).collect::<Vec<_>>(),
                              r.items.iter().filter(|i| i.folder == "drafts").count()),
            Err(e) => println!("FAIL related: {e}"),
        }
        if let Some(phrase) = args.get(pos + 2) {
            let q = inbox_chat::mail::SearchQuery { query: phrase.clone(), ..Default::default() };
            let exact = inbox_chat::mail::dasl_filter(&q, None).map(|_| ()).is_ok();
            println!("PASS phrase search in Sent Items (with fallback): {} result(s) [words used if needed: {}] {exact}",
                     o.search(&q, &["sent".into()], None, 10).map(|r| r.len()).unwrap_or(0),
                     inbox_chat::mail::fallback_words(phrase).len());
        }
        return;
    }
    if let Some(pos) = args.iter().position(|a| a == "--forward-dryrun") {
        let short = args.get(pos + 1).cloned().unwrap_or_default();
        let store = inbox_chat::store::Store::open(&inbox_chat::store::Store::default_path()).expect("open app database");
        let Some(eid) = store.entry_id(&short).ok().flatten() else {
            println!("FAIL unknown id {short}");
            std::process::exit(1);
        };
        let before = o.list_drafts(200).map(|d| d.len()).unwrap_or(0);
        let style = inbox_chat::text::BlockStyle { font_family: "Sakkal Majalla".into(), font_size_pt: 14.0, rtl: true };
        let text = "مع التحية والتقدير للجميع\n\nاختبار تجريبي — لن يُحفظ.\n\nTest message — not saved.";
        match o.dry_run_forward(&eid, text, style) {
            Ok((how, landed, class)) => println!("{} forward dry run on {short} (item class {class}): written via {how}, text landed={landed}",
                                                 if landed { "PASS" } else { "FAIL" }),
            Err(e) => println!("FAIL forward dry run: {e}"),
        }
        let after = o.list_drafts(200).map(|d| d.len()).unwrap_or(0);
        println!("{} drafts before={before} after={after} (nothing saved)", if before == after { "PASS" } else { "FAIL" });
        return;
    }
    let mut report = |name: &str, r: Result<String, String>| match r {
        Ok(s) => println!("PASS {name}: {s}"),
        Err(e) => {
            failures += 1;
            println!("FAIL {name}: {e}")
        }
    };

    report("classic Outlook process detected", if inbox_chat::outlook::outlook_running() { Ok("OUTLOOK.EXE running".into()) }
           else { Err("not running (the app would start it)".into()) });
    let folders = o.list_folders(2);
    report("list_folders", folders.as_ref().map(|f| format!("{} folders", f.len())).map_err(|e| e.to_string()));

    let recent = o.list_messages("inbox", Some(30), false, 5);
    report("list_messages(inbox, 30 days)", recent.as_ref().map(|m| {
        let with_date = m.iter().filter(|s| s.received.is_some()).count();
        let sorted = m.windows(2).all(|w| w[0].received >= w[1].received);
        let smtp = m.iter().filter(|s| s.from.contains('@')).count();
        format!("{} items, {with_date} dated, newest-first={sorted}, {smtp} with SMTP sender", m.len())
    }).map_err(|e| e.to_string()));

    report("list_messages(unread_only)", o.list_messages("inbox", Some(30), true, 5)
        .map(|m| format!("{} items, all unread={}", m.len(), m.iter().all(|s| s.unread))).map_err(|e| e.to_string()));

    if let Some(first) = recent.as_ref().ok().and_then(|m| m.first()) {
        report("get_message", o.get_message(&first.entry_id)
            .map(|m| format!("body {} chars, {} attachments", m.body.chars().count(), m.attachment_names.len()))
            .map_err(|e| e.to_string()));
        report("get_thread", o.get_thread(&first.entry_id, 12, 2500)
            .map(|t| format!("{} messages, oldest-first={}", t.len(), t.windows(2).all(|w| w[0].received <= w[1].received)))
            .map_err(|e| e.to_string()));
        report("read_full (reading pane)", o.read_full(&first.entry_id)
            .map(|m| format!("html {} chars, text {} chars, {} visible attachments, draft={}, inline images embedded={}, cid refs left={}",
                             m.html.chars().count(), m.text.chars().count(), m.attachments.len(), m.is_draft,
                             m.html.matches("data:image/").count(), m.html.to_ascii_lowercase().matches("cid:").count()))
            .map_err(|e| e.to_string()));
        report("read does not mark as read", o.list_messages("inbox", Some(30), false, 5)
            .map_err(|e| e.to_string())
            .and_then(|after| match after.iter().find(|s| s.entry_id == first.entry_id) {
                Some(s) if s.unread == first.unread => Ok(format!("unread flag unchanged ({})", s.unread)),
                Some(_) => Err("unread flag changed!".into()),
                None => Ok("item moved out of the first 5; skipped".into()),
            }));
    }

    let tree = o.folder_tree(6);
    report("folder_tree", tree.as_ref().map(|t| {
        let specials: Vec<String> = t.iter().filter_map(|n| n.special.clone()).collect();
        format!("{} top-level mail folders, defaults found: {}", t.len(), specials.join(","))
    }).map_err(|e| e.to_string()));
    report("page(inbox, 0, 50)", o.page("inbox", 0, 50)
        .map(|(items, more)| format!("{} items, has_more={more}", items.len())).map_err(|e| e.to_string()));
    report("page(drafts)", o.page("drafts", 0, 10)
        .map(|(items, _)| format!("{} drafts listed", items.len())).map_err(|e| e.to_string()));

    use inbox_chat::mail::SearchQuery;
    report("search(sender 'a', 30 days)", o.search(&SearchQuery { sender: "a".into(), ..Default::default() }, &[], Some(30), 5)
        .map(|r| format!("{} results", r.len())).map_err(|e| e.to_string()));
    report("search(Arabic keyword)", o.search(&SearchQuery { query: "ال".into(), ..Default::default() }, &[], Some(30), 5)
        .map(|r| format!("{} results", r.len())).map_err(|e| e.to_string()));
    report("search(recipient 'a' in Sent Items, 30 days)", o.search(&SearchQuery { recipient: "a".into(), ..Default::default() },
        &["sent".into()], Some(30), 5).map(|r| format!("{} results", r.len())).map_err(|e| e.to_string()));
    report("folder path error", match o.list_messages("Inbox/__no_such_folder__", None, false, 1) {
        Err(e) if e.to_string().contains("not found") => Ok("actionable error".into()),
        other => Err(format!("unexpected: {:?}", other.map(|v| v.len()))),
    });
    report("list_drafts", o.list_drafts(5).map(|d| format!("{} drafts", d.len())).map_err(|e| e.to_string()));

    // --llm: also exercise learning, recipient lookup and the insights panel with the user's configured LLM.
    // In-memory store (nothing saved, nothing written to Outlook); prints counts and lengths only.
    if std::env::args().any(|a| a == "--llm") {
        use inbox_chat::{agent::ChatAgent, settings::Settings, store::Store, tools::MailTools};
        use std::sync::Arc;
        let store = Arc::new(Store::memory().unwrap());
        // The LLM settings saved by the app (read only; keys decrypt for the same Windows user).
        let app_db = Store::open(&Store::default_path()).map(Arc::new).unwrap_or_else(|_| store.clone());
        let settings = Arc::new(Settings::load(app_db));
        let mail: Arc<dyn MailStore> = Arc::new(o);
        let agent = ChatAgent::new(MailTools::new(mail.clone(), store.clone()), settings.clone());
        let ctx = agent.tools.ctx.clone();
        let mut p = ctx.profile();
        p.learning.days_back = 90;
        p.learning.max_examples = 60;
        let t0 = std::time::Instant::now();
        report("learn (90 days, ≤60 replies)", inbox_chat::learn::learn(mail.as_ref(), &store, &p.style, &p.learning, &Default::default())
            .map(|r| format!("{} sent items read, {} replies learned, {} correspondents, {:.0}s", r.scanned, r.added,
                             store.contact_count().unwrap_or(0), t0.elapsed().as_secs_f64())).map_err(|e| e.to_string()));
        report("index inbox senders", inbox_chat::learn::index_senders(mail.as_ref(), &store, 90, 300)
            .map(|n| format!("{n} added, {} correspondents", store.contact_count().unwrap_or(0))).map_err(|e| e.to_string()));
        report("whoami", ctx.detected().map(|(n, e)| format!("name {} chars, email has @: {}", n.chars().count(), e.contains('@')))
            .ok_or_else(|| "not detected".to_string()));
        let top: Vec<_> = store.contacts(3).unwrap_or_default();
        if let Some(c) = top.first() {
            let first = c.name.split_whitespace().next().unwrap_or("").to_string();
            report("recipient lookup by first name of a frequent contact", Ok(format!("{} match(es)", ctx.match_contacts(&first).len())));
        }
        let cfg = settings.get();
        if let Some(first) = recent.as_ref().ok().and_then(|m| m.first()) {
            let t1 = std::time::Instant::now();
            report(&format!("insights via {}", cfg.provider), agent.insights(&first.entry_id, true)
                .map(|i| format!("summary {} chars, {} actions, needs_reply={}, reply {} chars, greeting_name {} chars, {:.0}s",
                                 i.summary.chars().count(), i.actions.len(), i.needs_reply, i.reply.chars().count(),
                                 i.greeting_name.chars().count(), t1.elapsed().as_secs_f64())));
        }
    }

    println!("\n{}", if failures == 0 { "All read-only checks passed." } else { "Some checks failed." });
    std::process::exit(if failures == 0 { 0 } else { 1 });
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Outlook self-test runs on Windows only.");
}
