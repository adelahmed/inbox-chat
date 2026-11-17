//! Everything the app knows about the user, shared by the tools, the agent and the UI commands:
//! the saved profile, who they are in Outlook, styled drafts, relevance tags and learned examples.

use crate::learn::{format_examples, median_words, similar};
use crate::mail::{split_recipients, MailStore, Summary};
use crate::profile::{classify, name_matches, translit_matches, DraftText, Profile, Relevance};
use crate::store::{Contact, Example, Store};
use crate::text::truncate_chars;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex, RwLock};

const KV_PROFILE: &str = "profile";

pub struct UserContext {
    pub store: Arc<Store>,
    pub mail: Arc<dyn MailStore>,
    profile: RwLock<Profile>,
    me: Mutex<Option<(String, String)>>,
}

impl UserContext {
    pub fn load(store: Arc<Store>, mail: Arc<dyn MailStore>) -> Self {
        let profile = store.kv_get(KV_PROFILE).ok().flatten()
            .and_then(|s| serde_json::from_str::<Profile>(&s).ok())
            .unwrap_or_default();
        UserContext { store, mail, profile: RwLock::new(profile), me: Mutex::new(None) }
    }

    pub fn profile(&self) -> Profile {
        self.profile.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Never saved on this PC (a new installation).
    pub fn never_saved(&self) -> bool {
        self.store.kv_get(KV_PROFILE).ok().flatten().is_none()
    }

    pub fn save(&self, p: Profile) -> Result<(), String> {
        let p = Self::normalize(p)?;
        let s = serde_json::to_string(&p).map_err(|e| e.to_string())?;
        self.store.kv_set(KV_PROFILE, &s).map_err(|e| format!("Could not save the profile: {e}"))?;
        *self.profile.write().unwrap_or_else(|e| e.into_inner()) = p;
        Ok(())
    }

    /// Tidy a profile and check it, without saving (also used to validate an import first).
    pub fn normalize(mut p: Profile) -> Result<Profile, String> {
        let clean = |v: &mut Vec<String>| { v.iter_mut().for_each(|s| *s = s.trim().to_string()); v.retain(|s| !s.is_empty()); };
        clean(&mut p.my_names);
        clean(&mut p.my_mentions);
        clean(&mut p.dept_keywords);
        clean(&mut p.dept_addresses);
        p.style.special.retain(|s| !s.matches.trim().is_empty() && !s.greeting.trim().is_empty());
        if p.style.font_family.trim().is_empty() {
            p.style.font_family = "Calibri".into();
        }
        p.language = p.language.trim().chars().take(40).collect();
        p.style.font_size_pt = p.style.font_size_pt.clamp(8.0, 36.0);
        if !p.style.greeting_single.contains("{name}") {
            return Err("The single-recipient greeting must contain {name}.".into());
        }
        p.learning.min_examples = p.learning.min_examples.clamp(1, 500);
        p.learning.days_back = p.learning.days_back.clamp(7, 3650);
        p.learning.max_examples = p.learning.max_examples.clamp(50, 5000);
        Ok(p)
    }

    /// Outlook's (display name, SMTP) for the mailbox owner; fetched once, retried while Outlook is down.
    pub fn detected(&self) -> Option<(String, String)> {
        let mut me = self.me.lock().unwrap_or_else(|e| e.into_inner());
        if me.is_none() {
            *me = self.mail.whoami().ok().filter(|(n, s)| !n.is_empty() || !s.is_empty());
        }
        me.clone()
    }

    /// Names/addresses that mean "the user" in To/CC lines.
    pub fn me(&self) -> Vec<String> {
        let mut v = self.profile().my_names;
        if let Some((n, s)) = self.detected() {
            v.extend([n, s].into_iter().filter(|x| !x.is_empty()));
        }
        v
    }

    pub fn draft(&self, body: &str, greeting_name: Option<String>) -> DraftText {
        let p = self.profile();
        DraftText { body: body.to_string(), greeting_name, style: Some(p.style), me: self.me() }
    }

    pub fn classify(&self, to: &str, cc: &str, subject: &str, body: &str) -> Relevance {
        classify(&self.profile(), &self.me(), to, cc, subject, body)
    }

    pub fn tag(&self, rows: &mut [Summary]) {
        let (p, me) = (self.profile(), self.me());
        for r in rows {
            r.tags = classify(&p, &me, &r.to, &r.cc, &r.subject, &r.body_head).tags;
        }
    }

    /// Correspondents matching `name` (every word in their name or address), most frequent first,
    /// excluding the user.
    pub fn match_contacts(&self, name: &str) -> Vec<Contact> {
        let me = self.me();
        self.store.contacts(5000).unwrap_or_default().into_iter()
            .filter(|c| !me.iter().any(|m| m.eq_ignore_ascii_case(&c.email) || name_matches(&c.name, m)))
            .filter(|c| name_matches(&c.name, name) || name_matches(c.email.split('@').next().unwrap_or(""), name)
                        || translit_matches(&c.name, name))
            .collect()
    }

    /// Turn names into addresses before Outlook resolves them: an address is kept; a name matching
    /// exactly one correspondent becomes their address; several matches are never guessed (the name is
    /// kept and the candidates returned); no match is left for Outlook's address book.
    pub fn resolve_recipients(&self, field: &str) -> (String, Vec<(String, Vec<Contact>)>) {
        let mut out = Vec::new();
        let mut ambiguous = Vec::new();
        for name in split_recipients(field) {
            if name.contains('@') {
                out.push(name);
                continue;
            }
            let m = self.match_contacts(&name);
            match m.len() {
                1 => out.push(m[0].email.clone()),
                0 => out.push(name),
                _ => {
                    ambiguous.push((name.clone(), m.into_iter().take(5).collect()));
                    out.push(name);
                }
            }
        }
        (out.join("; "), ambiguous)
    }

    /// For the find_recipient tool.
    pub fn find_recipient(&self, name: &str) -> Value {
        let hits = self.match_contacts(name);
        if !hits.is_empty() {
            return json!({"matches": hits.into_iter().take(8).collect::<Vec<_>>()});
        }
        // Not among correspondents (or they aren't indexed yet): ask Outlook's address book; only a
        // unique match counts.
        if let Ok(Some((n, e))) = self.mail.resolve_name(name) {
            return json!({"matches": [{"name": n, "email": e, "count": 0, "source": "address book"}]});
        }
        let me = self.me();
        let frequent: Vec<Contact> = self.store.contacts(40).unwrap_or_default().into_iter()
            .filter(|c| !me.iter().any(|m| m.eq_ignore_ascii_case(&c.email))).take(15).collect();
        json!({"matches": [], "frequent_contacts": frequent,
               "note": "No direct match; the name may be spelled differently (Arabic/English). Pick from \
                        frequent_contacts only if clearly the same person, otherwise ask the user."})
    }

    pub fn example_count(&self) -> i64 {
        self.store.example_count().unwrap_or(0)
    }

    pub fn examples_for(&self, from: &str, subject: &str, text: &str, k: usize) -> Vec<Example> {
        let all = self.store.examples(self.profile().learning.max_examples).unwrap_or_default();
        similar(&all, from, subject, text, k)
    }

    /// The part of the system prompt about the user. `about` = (from, subject, text) of the email in
    /// focus, used to pick the most similar past replies.
    pub fn prompt_section(&self, about: Option<(&str, &str, &str)>) -> String {
        let p = self.profile();
        let name = self.detected().map(|(n, _)| n).filter(|n| !n.is_empty()).unwrap_or_else(|| "the mailbox owner".into());
        let mut s = format!("\n\nAbout the user:\n- {name}{}{}.",
            if p.job_title.is_empty() { String::new() } else { format!(", {}", p.job_title) },
            if p.dept_name.is_empty() { String::new() } else { format!(", {}", p.dept_name) });
        if !p.background.trim().is_empty() {
            s.push_str(&format!("\n\nDepartment background (use it to understand requests; don't recite it):\n{}",
                                truncate_chars(p.background.trim(), 3500)));
        }
        s.push_str("\n\nPriorities: emails tagged to_you, mentions_you, department or request are addressed to the user or \
                    the department. When asked what needs attention, lead with those.");
        if p.style.enabled {
            let special: Vec<String> = p.style.special.iter()
                .map(|x| format!("«{}» for {}", x.greeting, x.matches.split('|').next().unwrap_or(""))).collect();
            s.push_str(&format!(
                "\n\nWriting style — the app adds these automatically to every draft, so do NOT write them yourself:\n\
                 - greeting «{single}» (one recipient), «{multi}» (several){special};\n\
                 - sign-off «{sign}»; font {font}{dir}.\n\
                 In create_*_draft `body`, write only the message itself: no greeting, no sign-off, no signature, and \
                 do not open with another salutation (such as «Dear …», «Hi …» or «السلام عليكم») — the greeting covers it.\n\
                 For one recipient pass `greeting_name`: how the user addresses them, with a title if the user uses one \
                 (e.g. \"Dr. Sara\", \"الأستاذ خالد\").",
                single = p.style.greeting_single.replace("{name}", "<name>"), multi = p.style.greeting_multi,
                special = if special.is_empty() { String::new() } else { format!(", {}", special.join(", ")) },
                sign = p.style.signoff.replace('\n', " "), font = p.style.font_family,
                dir = if p.style.rtl { ", right-to-left" } else { "" }));
        }
        s.push_str(&match p.language.trim() {
            "" => "\n\nWrite drafts in the language of the email being answered unless the user asks otherwise.".to_string(),
            lang => format!("\n\nWrite drafts in {lang} unless the user asks for another language."),
        });
        if p.learning.enabled && self.example_count() > 0 {
            let (from, subject, text) = about.unwrap_or(("", "", ""));
            let ex = self.examples_for(from, subject, text, 3);
            let all = self.store.examples(300).unwrap_or_default();
            s.push_str(&format!(
                "\n\nThe user's replies are usually about {} words. Past replies by the user (imitate tone, length and \
                 phrasing, never copy facts):\n{}", median_words(&all), format_examples(&ex)));
        }
        s
    }

    /// Profile for the UI, plus what was detected and learned.
    pub fn public(&self) -> Value {
        let detected = self.detected();
        json!({
            "profile": self.profile(),
            "defaults": Profile::default(),
            "detected_name": detected.as_ref().map(|d| d.0.clone()),
            "detected_email": detected.as_ref().map(|d| d.1.clone()),
            "examples": self.example_count(),
        })
    }
}
