//! The user's profile: who they are, their department's background, how they write, and what makes an
//! email "for them". Writing style is applied by code (greeting, sign-off, font, direction), not left
//! to the model, so drafts are consistent whatever LLM is used.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;


#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SpecialAddressee {
    /// Names/addresses that identify the person, separated by '|', e.g. "طارق الناصر|Tariq Alnasser".
    pub matches: String,
    /// The full greeting line used when this person is the only recipient.
    pub greeting: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Style {
    pub enabled: bool,
    /// `{name}` is replaced by how the single recipient is addressed ("ل{name}" joins Arabic-style: للأستاذ…).
    pub greeting_single: String,
    pub greeting_multi: String,
    pub special: Vec<SpecialAddressee>,
    pub signoff: String,
    pub font_family: String,
    pub font_size_pt: f32,
    pub rtl: bool,
}

impl Default for Style {
    fn default() -> Self {
        // Off until the user sets up their own greeting and sign-off in Profile → Writing style.
        Style {
            enabled: false,
            greeting_single: "Dear {name},".into(),
            greeting_multi: "Dear all,".into(),
            special: vec![],
            signoff: "Best regards".into(),
            font_family: "Calibri".into(),
            font_size_pt: 11.0,
            rtl: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Learning {
    pub enabled: bool,
    /// Suggest a reply automatically when an email for you is opened (once enough replies are learned).
    pub auto_suggest: bool,
    pub min_examples: usize,
    pub days_back: i64,
    pub max_examples: usize,
}

impl Default for Learning {
    fn default() -> Self {
        Learning { enabled: true, auto_suggest: true, min_examples: 10, days_back: 365, max_examples: 1000 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Profile {
    /// How the user appears in To/CC lines (Outlook display names, addresses). Outlook's own
    /// CurrentUser is added automatically.
    pub my_names: Vec<String>,
    /// Short forms that mean "you" when written in a body ("Dr. Sam", "د. سامي").
    pub my_mentions: Vec<String>,
    pub job_title: String,
    pub dept_name: String,
    pub dept_keywords: Vec<String>,
    /// Department distribution lists / shared mailboxes (names or addresses).
    pub dept_addresses: Vec<String>,
    pub background: String,
    /// Language for summaries, suggestions and drafts, e.g. "Arabic"; empty = the language of the email.
    pub language: String,
    pub style: Style,
    pub learning: Learning,
    /// Mark emails as read in Outlook when opened in the reading pane or processed by the assistant.
    pub mark_read: bool,
}

impl Default for Profile {
    fn default() -> Self {
        Profile {
            my_names: vec![],
            my_mentions: vec![],
            job_title: String::new(),
            dept_name: String::new(),
            dept_keywords: vec![],
            dept_addresses: vec![],
            background: String::new(),
            language: String::new(),
            style: Style::default(),
            learning: Learning::default(),
            mark_read: true,
        }
    }
}

// --------------------------------------------------------------------------- Arabic-aware matching
static DIACRITICS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\u{064B}-\u{065F}\u{0670}\u{0640}]").unwrap());

/// Lowercase; strip Arabic diacritics and tatweel; unify alef/yaa/taa-marbuta; drop '-' and '.'.
pub fn norm(s: &str) -> String {
    let s = DIACRITICS.replace_all(s, "");
    s.to_lowercase().chars().filter_map(|c| match c {
        'أ' | 'إ' | 'آ' | 'ٱ' => Some('ا'),
        'ى' => Some('ي'),
        'ة' => Some('ه'),
        'ؤ' => Some('و'),
        'ئ' => Some('ي'),
        '-' | '.' | '_' => None,
        c => Some(c),
    }).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

/// True when every word of `alias` appears in `text` (after normalisation).
pub fn name_matches(text: &str, alias: &str) -> bool {
    let t = norm(text);
    let words: Vec<String> = norm(alias).split(' ').filter(|w| !w.is_empty()).map(String::from).collect();
    !words.is_empty() && words.iter().all(|w| t.contains(w.as_str()))
}

/// A name word as consonants plus the vowel letters in each gap (gap 0 = before the first consonant).
/// Arabic vowels: ا → 'a', ي → 'i', و → 'u' (ى/ة at the end → 'a'); ع/ء are ignored.
fn shape(word: &str, strip_final_h: bool) -> Option<(bool, Vec<String>, Vec<String>)> {
    // Lighter than norm(): keeps ى and ة, which carry vowel information here.
    let mut w: String = DIACRITICS.replace_all(word, "").to_lowercase().chars().filter_map(|c| match c {
        'أ' | 'إ' | 'آ' | 'ٱ' => Some('ا'),
        'ؤ' => Some('و'),
        'ئ' => Some('ي'),
        '-' | '.' | '_' | ' ' => None,
        c => Some(c),
    }).collect();
    let latin = w.chars().any(|c| c.is_ascii_alphabetic());
    if latin && strip_final_h && w.len() > 2 && w.ends_with('h') && w[..w.len() - 1].ends_with(['a', 'e', 'i', 'o', 'u']) {
        w.pop(); // "Sarah" ≈ سارة, "Abdullah" ≈ عبدالله (but "Saleh" keeps its ح: both forms are tried)
    }
    let (mut cons, mut gaps): (Vec<String>, Vec<String>) = (vec![], vec![String::new()]);
    let push = |c: &str, cons: &mut Vec<String>, gaps: &mut Vec<String>| {
        if cons.last().is_some_and(|l| l == c) && gaps.last().is_some_and(|g| g.is_empty()) {
            return; // doubled letter (Mohammed, Hassan, الله)
        }
        cons.push(c.to_string());
        gaps.push(String::new());
    };
    if latin {
        let chars: Vec<char> = w.chars().filter(|c| c.is_ascii_alphabetic()).collect();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            let next = chars.get(i + 1).copied();
            if "aeiouyw".contains(c) {
                gaps.last_mut().unwrap().push(c);
            } else if next == Some('h') && "skgtd".contains(c) {
                let dig = match c { 's' => "sh", 'k' => "kh", 'g' => "gh", _ => "th" };
                push(dig, &mut cons, &mut gaps);
                i += 1;
            } else {
                push(&c.to_string(), &mut cons, &mut gaps);
            }
            i += 1;
        }
    } else {
        for c in w.chars() {
            let s = match c {
                'ب' => "b", 'ت' | 'ط' => "t", 'ث' | 'ذ' | 'ظ' => "th", 'ج' => "j", 'ح' | 'ه' => "h", 'خ' => "kh", 'د' | 'ض' => "d",
                'ر' => "r", 'ز' => "z", 'س' | 'ص' => "s", 'ش' => "sh", 'غ' => "gh", 'ف' => "f", 'ق' => "q", 'ك' => "k",
                'ل' => "l", 'م' => "m", 'ن' => "n",
                'ا' | 'ى' | 'ة' => { gaps.last_mut().unwrap().push('a'); continue; }
                'ي' => { gaps.last_mut().unwrap().push('i'); continue; }
                'و' => { gaps.last_mut().unwrap().push('u'); continue; }
                _ => continue, // ع ء and anything else
            };
            push(s, &mut cons, &mut gaps);
        }
    }
    (cons.len() >= 2).then_some((latin, cons, gaps))
}

/// Same name across Arabic and Latin spelling: same consonants, and long vowels agree
/// (حسين has ي, so it is not "Hassan"; "Hussain" has "ai", so it is not حسن). Gap 0 is not compared.
fn translit_word(a: &str, b: &str) -> bool {
    [false, true].iter().any(|&strip| {
        let (Some((la, ca, ga)), Some((lb, cb, gb))) = (shape(a, strip), shape(b, strip)) else { return false };
        if la == lb || ca != cb {
            return false; // same script is handled by plain matching
        }
        let (ar, lat) = if la { (gb, ga) } else { (ga, gb) };
        ar.iter().zip(lat.iter()).skip(1).all(|(a, l)| {
            // ي needs i/y/e in the Latin spelling, و needs u/o/w; ا (often silent in ال…) needs nothing.
            let ok_i = !a.contains('i') || l.contains(['i', 'y', 'e']);
            let ok_u = !a.contains('u') || l.contains(['u', 'o', 'w']);
            // A long Latin vowel ("ai", "ou", "ee", "y", "w") needs a long vowel letter in the Arabic.
            let long = l.chars().count() >= 2 || l.contains(['y', 'w']);
            ok_i && ok_u && (!long || !a.is_empty())
        })
    })
}

/// Every word of `query` matches some word of `name` across Arabic/Latin spellings
/// (منى ≈ Mona, محمد ≈ Mohammed, الخطيب ≈ Al-Khatib).
pub fn translit_matches(name: &str, query: &str) -> bool {
    // Split the raw text: shape() does its own normalisation and needs ى/ة intact.
    let words: Vec<&str> = name.split_whitespace().collect();
    let q: Vec<&str> = query.split_whitespace().collect();
    !q.is_empty() && q.iter().all(|w| words.iter().any(|n| translit_word(n, w)))
}

fn contains_norm(hay_norm: &str, needle: &str) -> bool {
    let n = norm(needle);
    !n.is_empty() && hay_norm.contains(&n)
}

// --------------------------------------------------------------------------- writing style
/// "ل" + name with Arabic joining: ل + ال… → لل…; Latin names get "لـ ".
pub fn lam(name: &str) -> String {
    let name = name.trim();
    if let Some(rest) = name.strip_prefix("ال") {
        return format!("لل{rest}");
    }
    match name.chars().next() {
        Some(c) if c.is_ascii_alphabetic() => format!("لـ {name}"),
        _ => format!("ل{name}"),
    }
}

/// The greeting line for a draft, from its To recipients (display names, excluding the user).
pub fn greeting(style: &Style, to: &[String], greeting_name: Option<&str>) -> String {
    let named = greeting_name.map(str::trim).filter(|n| !n.is_empty());
    if to.len() > 1 {
        return style.greeting_multi.clone();
    }
    let candidates: Vec<&str> = to.iter().map(String::as_str).chain(named).collect();
    for sp in &style.special {
        if candidates.iter().any(|c| sp.matches.split('|').any(|alias| name_matches(c, alias))) {
            return sp.greeting.clone();
        }
    }
    match named.or(to.first().map(String::as_str)) {
        Some(name) => {
            // Template is "...ل{name}": replace "ل{name}" so the joining rule applies.
            if style.greeting_single.contains("ل{name}") {
                style.greeting_single.replace("ل{name}", &lam(name))
            } else {
                style.greeting_single.replace("{name}", name)
            }
        }
        None => style.greeting_multi.clone(),
    }
}

/// The fixed opening of a greeting template: "مع التحية والتقدير ل{name}" → "مع التحية والتقدير",
/// "Dear {name}," → "dear", "Dear all," → "dear all,".
fn greeting_prefix(template: &str) -> String {
    let head = template.split("{name}").next().unwrap_or("").trim_end();
    let head = head.strip_suffix('ل').unwrap_or(head).trim_end();
    let head = head.split(" ل").next().unwrap_or(head); // "…للجميع" → "…"
    let n = norm(head);
    if n.chars().count() >= 3 { n } else { String::new() }
}

/// Remove a greeting/sign-off the model wrote itself, so ours isn't duplicated.
pub fn strip_own_greeting_signoff(style: &Style, body: &str) -> String {
    let mut lines: Vec<&str> = body.trim().lines().collect();
    let prefixes: Vec<String> = [&style.greeting_single, &style.greeting_multi].iter().map(|t| greeting_prefix(t))
        .filter(|p| !p.is_empty()).collect();
    // The user's greeting replaces salaam openers too («وعليكم السلام …», «السلام عليكم …»).
    let salaam = [norm("وعليكم السلام"), norm("السلام عليكم")];
    while lines.first().is_some_and(|l| {
        let n = norm(l);
        let short = n.chars().count() <= 60;
        n.is_empty() || (short && prefixes.iter().any(|p| n.starts_with(p.as_str())))
            || (short && salaam.iter().any(|s| n.starts_with(s.as_str())))
    }) {
        lines.remove(0);
    }
    let sign: Vec<String> = style.signoff.lines().map(norm).filter(|l| !l.is_empty()).collect();
    while let Some(last) = lines.last() {
        let n = norm(last);
        if n.is_empty() || sign.iter().any(|s| n == *s || n.trim_start_matches('-').trim() == s.trim_start_matches('-').trim()) {
            lines.pop();
        } else {
            break;
        }
    }
    lines.join("\n").trim().to_string()
}

/// What the model (or a suggestion) wrote, plus how to style it. Rendering needs the draft's actual
/// To recipients, which are only known once Outlook has built the reply, so it happens in the mail layer.
#[derive(Debug, Clone, Default)]
pub struct DraftText {
    pub body: String,
    pub greeting_name: Option<String>,
    pub style: Option<Style>,
    /// The user's own names/addresses, removed from the recipient list before choosing the greeting.
    pub me: Vec<String>,
}

impl DraftText {
    pub fn plain(body: &str) -> Self {
        DraftText { body: body.into(), ..Default::default() }
    }

    /// Final text (greeting + body + sign-off) for these To recipients.
    pub fn render(&self, to: &[String]) -> String {
        let Some(style) = self.style.as_ref().filter(|s| s.enabled) else { return self.body.trim().to_string() };
        let others: Vec<String> = to.iter().filter(|n| !self.me.iter().any(|m| name_matches(n, m) || name_matches(m, n)))
            .cloned().collect();
        let body = strip_own_greeting_signoff(style, &self.body);
        format!("{}\n\n{}\n\n{}", greeting(style, &others, self.greeting_name.as_deref()), body, style.signoff.trim())
    }

    pub fn block_style(&self) -> crate::text::BlockStyle {
        match self.style.as_ref().filter(|s| s.enabled) {
            Some(s) => crate::text::BlockStyle { font_family: s.font_family.clone(), font_size_pt: s.font_size_pt, rtl: s.rtl },
            None => crate::text::BlockStyle::default(),
        }
    }
}

// --------------------------------------------------------------------------- relevance
const REQUEST_PHRASES: &[&str] = &[
    "نأمل", "نامل", "نرجو", "يرجى", "الرجاء", "أرجو", "ارجو", "آمل", "التكرم", "تكرمكم", "نود", "نحتاج", "مطلوب", "المطلوب",
    "الموافقة", "اعتماد", "للاعتماد", "التوجيه", "توجيهكم", "إفادتنا", "افادتنا", "الإفادة", "الافادة", "موافاتنا", "عاجل",
    "بحلول", "قبل نهاية", "في موعد أقصاه", "please", "kindly", "could you", "can you", "would you", "request", "approval",
    "approve", "your input", "your feedback", "deadline", "by end of", "asap", "urgent", "action required",
];

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Relevance {
    /// Subset of: to_you, cc_you, mentions_you, department, request.
    pub tags: Vec<String>,
    /// Department keywords / names that matched (shown in the reading pane).
    pub matched: Vec<String>,
}

impl Relevance {
    pub fn for_you(&self) -> bool {
        self.tags.iter().any(|t| t == "to_you" || t == "mentions_you" || t == "department")
    }
}

/// Classify an email. `me` = profile names + Outlook's CurrentUser name/address.
pub fn classify(p: &Profile, me: &[String], to: &str, cc: &str, subject: &str, body: &str) -> Relevance {
    let mut r = Relevance::default();
    let list_has = |field: &str, who: &[String]| {
        field.split(';').any(|entry| who.iter().any(|w| !w.trim().is_empty() && (name_matches(entry, w) || name_matches(w, entry.trim()))))
    };
    if list_has(to, me) {
        r.tags.push("to_you".into());
    } else if list_has(cc, me) {
        r.tags.push("cc_you".into());
    }
    let text = norm(&format!("{subject}\n{body}"));
    if p.my_mentions.iter().any(|m| contains_norm(&text, m)) {
        r.tags.push("mentions_you".into());
    }
    for k in p.dept_keywords.iter().chain(std::iter::once(&p.dept_name)) {
        if contains_norm(&text, k) && !r.matched.contains(k) {
            r.matched.push(k.clone());
        }
    }
    let dl_hit = !p.dept_addresses.is_empty() && (list_has(to, &p.dept_addresses) || list_has(cc, &p.dept_addresses));
    if dl_hit || !r.matched.is_empty() {
        r.tags.push("department".into());
    }
    // A request only matters when the email is for the user or the department.
    let asks = REQUEST_PHRASES.iter().any(|ph| contains_norm(&text, ph)) || text.contains('؟') || text.contains('?');
    if asks && r.for_you() {
        r.tags.push("request".into());
    }
    r.matched.truncate(5);
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st() -> Style {
        crate::fake_mail::sample_profile().style
    }

    #[test]
    fn greeting_rules() {
        let s = st();
        assert_eq!(greeting(&s, &["Khalid Mansour".into()], Some("الأستاذ خالد")), "مع التحية والتقدير للأستاذ خالد", "ل+ال → لل");
        assert_eq!(greeting(&s, &["Khalid Mansour".into()], Some("خالد")), "مع التحية والتقدير لخالد");
        assert_eq!(greeting(&s, &["Khalid Mansour".into()], None), "مع التحية والتقدير لـ Khalid Mansour", "Latin name");
        assert_eq!(greeting(&s, &["A".into(), "B".into()], Some("خالد")), "مع التحية والتقدير للجميع");
        assert_eq!(greeting(&s, &["Tariq Al-Nasser".into()], None), "مع التحية والتقدير لسعادة الرئيس التنفيذي");
        assert_eq!(greeting(&s, &["طارق بن عمر الناصر".into()], None), "مع التحية والتقدير لسعادة الرئيس التنفيذي");
        assert_eq!(greeting(&s, &["x@example.com".into()], Some("طارق الناصر")), "مع التحية والتقدير لسعادة الرئيس التنفيذي");
        assert_eq!(greeting(&s, &["طارق الناصر".into(), "Sara".into()], None), "مع التحية والتقدير للجميع", "special only when sole recipient");
        assert_eq!(greeting(&s, &["Tariq Hamdan".into()], None), "مع التحية والتقدير لـ Tariq Hamdan", "other Tariq");
    }

    #[test]
    fn generic_default_style() {
        let s = Style::default();
        assert!(!s.enabled, "no greeting or sign-off until the user sets one up");
        assert_eq!(DraftText { body: "Fine.".into(), style: Some(s.clone()), ..Default::default() }.render(&["Sara".into()]), "Fine.");
        let on = Style { enabled: true, ..s };
        assert_eq!(greeting(&on, &["Sara Ali".into()], Some("Sara")), "Dear Sara,");
        assert_eq!(greeting(&on, &["A".into(), "B".into()], None), "Dear all,");
        assert_eq!(strip_own_greeting_signoff(&on, "Dear Sara,\n\nWednesday works.\n\nBest regards"), "Wednesday works.");
        let d = DraftText { body: "Hi.".into(), style: Some(on), ..Default::default() };
        assert_eq!(d.render(&["Sara Ali".into()]), "Dear Sara Ali,\n\nHi.\n\nBest regards");
        let p = Profile::default();
        assert!(p.my_mentions.is_empty() && p.dept_name.is_empty() && p.background.is_empty() && p.dept_keywords.is_empty(),
                "nobody's personal details are built in");
    }

    #[test]
    fn render_adds_greeting_signoff_once_and_excludes_me() {
        let d = DraftText {
            body: "مع التحية والتقدير لخالد\n\nالأربعاء مناسب.\n\nمع تحياتي\n-عادل".into(),
            greeting_name: Some("خالد".into()), style: Some(st()), me: vec!["Adel Ahmed".into()],
        };
        assert_eq!(d.render(&["Khalid Mansour".into(), "Adel Ahmed".into()]), "مع التحية والتقدير لخالد\n\nالأربعاء مناسب.\n\nمع تحياتي\n-عادل");
        assert_eq!(DraftText::plain("hi").render(&["A".into()]), "hi", "no style = untouched");
        assert_eq!(strip_own_greeting_signoff(&st(), "وعليكم السلام الأستاذ خالد،\nيناسبني الأربعاء."), "يناسبني الأربعاء.");
        let b = d.block_style();
        assert!(b.rtl && b.font_family == "Sakkal Majalla");
    }

    #[test]
    fn relevance_tags() {
        let p = Profile { dept_keywords: vec!["الخدمات المتميزة".into(), "Customer experience".into()], ..crate::fake_mail::sample_profile() };
        let me = vec!["Adel Ahmed".into(), "adel@example.com".into()];
        let r = classify(&p, &me, "Adel Ahmed", "", "Budget", "Can you approve this?");
        assert_eq!(r.tags, ["to_you", "request"]);
        let r = classify(&p, &me, "Sara; Omar", "Adel Ahmed", "x", "FYI");
        assert_eq!(r.tags, ["cc_you"]);
        let r = classify(&p, &me, "Team", "", "طلب", "نرجو من إدارة الخدمات المتميّزة تزويدنا بالبيانات");
        assert_eq!(r.tags, ["department", "request"], "Arabic normalisation (shadda) + request");
        assert_eq!(r.matched, ["الخدمات المتميزة"]);
        let r = classify(&p, &me, "Team", "", "x", "سعادة د. عادل، للاطلاع");
        assert_eq!(r.tags, ["mentions_you"]);
        let r = classify(&p, &me, "Team", "", "Newsletter", "Can you believe it?");
        assert!(r.tags.is_empty(), "a question to someone else is not your request");
    }

    #[test]
    fn arabic_latin_names_match() {
        for (ar, en) in [("منى", "Mona Al-Khatib"), ("محمد", "Mohammed Fares"), ("خالد", "Khalid Mansour"),
                         ("رانيا", "Rania Saeed"), ("حسن العطار", "Hassan Alattar"), ("سارة المصري", "Sara Al-Masri"),
                         ("عمر", "Omar Saleh"), ("نورة", "Noura Aziz"), ("الخطيب", "Mona Al-Khatib")] {
            assert!(translit_matches(en, ar), "{ar} ≈ {en}");
        }
        for (ar, en) in [("سارة", "Sarah"), ("عبدالله", "Abdullah"), ("صالح", "Saleh"), ("أحمد", "Ahmed"), ("سعيد", "Saeed"),
                         ("عثمان", "Othman"), ("حسين", "Hussain")] {
            assert!(translit_matches(en, ar), "{ar} ≈ {en}");
        }
        assert!(!translit_matches("Khalid Mansour", "منى"));
        assert!(!translit_matches("Hassan Alattar", "حسين"), "Hassan ≠ حسين");
        assert!(!translit_matches("Hussain Fares", "حسن"), "Hussain ≠ حسن");
        assert!(translit_matches("منى الخطيب", "Mona"), "works the other way round too");
    }

}
