//! Text helpers shared by the Outlook layer and the tests: body cleanup, quoted-history stripping,
//! and inserting the user's text into an HTML reply above the quoted original.

use regex::Regex;
use std::sync::LazyLock;

static QUOTE_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        Regex::new(r"\n_{10,}\s*\n").unwrap(),
        Regex::new(r"(?i)\n-{2,}\s*Original Message\s*-{2,}").unwrap(),
        // English and Arabic Outlook reply headers: "From: … / Sent: …", "من: … / تاريخ الإرسال: …"
        Regex::new(r"(?i)\n(?:From|من)\s*:\s.*\n\s*(?:Sent|Date|تاريخ الإرسال|أرسل|التاريخ)\s*:").unwrap(),
    ]
});
static TRAILING_WS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[ \t\u{a0}]+\n").unwrap());
static MANY_NEWLINES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n{3,}").unwrap());
static PARA_SPLIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n\s*\n").unwrap());
static BODY_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<body[^>]*>").unwrap());

/// Normalise line endings, trim trailing spaces, collapse blank lines.
pub fn clean_text(text: &str) -> String {
    let t = text.replace("\r\n", "\n").replace('\r', "\n");
    let t = TRAILING_WS.replace_all(&t, "\n");
    let t = MANY_NEWLINES.replace_all(&t, "\n\n");
    t.trim().to_string()
}

/// Byte offset where the quoted earlier message starts in `text`, if any.
pub fn quote_start(text: &str) -> Option<usize> {
    let hay = format!("\n{text}");
    QUOTE_PATTERNS.iter().filter_map(|p| p.find(&hay)).map(|m| m.start().saturating_sub(1)).min()
}

/// Remove the quoted earlier messages that Outlook appends below a reply.
pub fn strip_quoted(text: &str) -> String {
    text[..quote_start(text).unwrap_or(text.len())].trim().to_string()
}

static HTML_DROP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<(script|style|head)\b.*?</(script|style|head)\s*>|<!--.*?-->").unwrap());
static HTML_BREAK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<br\s*/?>|</(p|div|tr|li|h[1-6]|table|blockquote)\s*>").unwrap());
static HTML_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<[^>]*>").unwrap());
static NUM_ENTITY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"&#(x?)([0-9a-fA-F]+);").unwrap());

/// Plain text from an HTML body, for emails whose `Body` is empty (HTML-only or protected mail).
pub fn html_to_text(html: &str) -> String {
    let t = HTML_DROP.replace_all(html, "");
    let t = HTML_BREAK.replace_all(&t, "\n");
    let t = HTML_TAG.replace_all(&t, "");
    let t = NUM_ENTITY.replace_all(&t, |c: &regex::Captures| {
        let radix = if c[1].is_empty() { 10 } else { 16 };
        u32::from_str_radix(&c[2], radix).ok().and_then(char::from_u32).map(String::from).unwrap_or_default()
    });
    let t = t.replace("&nbsp;", " ").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"")
        .replace("&#39;", "'").replace("&amp;", "&");
    clean_text(&t)
}

/// `Body`, or text derived from `HTMLBody` when `Body` is empty.
pub fn body_or_html(body: &str, html: impl FnOnce() -> String) -> String {
    let b = clean_text(body);
    if !b.is_empty() {
        return b;
    }
    html_to_text(&html())
}

/// First `n` characters (not bytes) of `s`.
pub fn truncate_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

pub fn char_len(s: &str) -> usize {
    s.chars().count()
}

pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(c),
        }
    }
    out
}

fn text_to_html(text: &str) -> String {
    PARA_SPLIT
        .split(text.trim())
        .map(|p| format!(r#"<p dir="auto" style="margin:0 0 10px 0">{}</p>"#, escape_html(p).replace('\n', "<br>")))
        .collect()
}

/// Font and direction of the text block the app writes into a draft.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockStyle {
    pub font_family: String,
    pub font_size_pt: f32,
    /// Right-to-left block (each paragraph keeps dir="auto", so an English line still reads correctly).
    pub rtl: bool,
}

impl Default for BlockStyle {
    fn default() -> Self {
        BlockStyle { font_family: "Calibri".into(), font_size_pt: 11.0, rtl: false }
    }
}

/// Put our text at the top of an Outlook reply, above the quoted original. `dir="auto"` lets Arabic
/// render right-to-left.
pub fn insert_into_html_body(existing_html: Option<&str>, text: &str) -> String {
    insert_styled(existing_html, text, &BlockStyle::default())
}

/// Same as `insert_into_html_body`, with the user's font and direction.
pub fn insert_styled(existing_html: Option<&str>, text: &str, s: &BlockStyle) -> String {
    let font: String = s.font_family.chars().filter(|c| !"<>\"';{}".contains(*c)).collect();
    let (dir, align) = if s.rtl { ("rtl", "text-align:right;") } else { ("auto", "") };
    let block = format!(
        r#"<div dir="{dir}" style="font-family:'{font}',Arial,sans-serif;font-size:{size}pt;{align}">{}</div>"#,
        text_to_html(text),
        size = s.font_size_pt,
    );
    let existing = existing_html.unwrap_or("");
    match BODY_TAG.find(existing) {
        Some(m) => format!("{}{}{}", &existing[..m.end()], block, &existing[m.end()..]),
        None => format!("{block}{existing}"),
    }
}

pub fn has_arabic(s: &str) -> bool {
    s.chars().any(|c| ('\u{0600}'..='\u{06FF}').contains(&c))
}

/// The details block of a forwarded meeting invitation (title, time, place): Arabic labels and
/// right-to-left when `arabic`, otherwise English.
pub fn invitation_details(subject: &str, when: Option<&str>, place: Option<&str>, arabic: bool, font: &str) -> String {
    let (inv, at, wh) = if arabic { ("الدعوة المرفقة:", "الموعد:", "المكان:") } else { ("Attached invitation:", "When:", "Where:") };
    let mut lines = vec![format!("<b>{inv}</b> {}", escape_html(subject))];
    if let Some(w) = when {
        lines.push(format!("<b>{at}</b> <span dir=\"ltr\">{}</span>", escape_html(w)));
    }
    if let Some(p) = place {
        lines.push(format!("<b>{wh}</b> {}", escape_html(p)));
    }
    let font: String = font.chars().filter(|c| !"<>\"';{}".contains(*c)).collect();
    format!("<div dir=\"{}\" style=\"font-family:'{font}',Arial,sans-serif;font-size:12pt;color:#444;\
             border-top:1px solid #ccc;margin-top:12px;padding-top:6px\">{}</div>",
            if arabic { "rtl" } else { "ltr" }, lines.join("<br>"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invitation_details_follow_the_language() {
        let ar = invitation_details("ورشة", Some("2026-11-18 10:00"), Some("القاعة <1>"), true, "Sakkal Majalla");
        assert!(ar.contains("dir=\"rtl\"") && ar.contains("الدعوة المرفقة:") && ar.contains("القاعة &lt;1&gt;"), "{ar}");
        let en = invitation_details("Workshop", None, None, false, "Calibri");
        assert!(en.contains("dir=\"ltr\"") && en.contains("<b>Attached invitation:</b> Workshop") && !en.contains("When:"), "{en}");
        assert!(has_arabic("مرحبا") && !has_arabic("hello"));
    }

    #[test]
    fn strips_english_and_arabic_quotes() {
        assert_eq!(strip_quoted("hello\n\nمن: خالد\nتاريخ الإرسال: الأحد\nold"), "hello");
        assert_eq!(strip_quoted("ok\n\nFrom: Sara\nSent: Friday\nquoted"), "ok");
        assert_eq!(strip_quoted("x\n________________________________\nFrom: a\nSent: b\nold"), "x");
        assert_eq!(strip_quoted("y\n----- Original Message -----\nold"), "y");
        assert_eq!(strip_quoted("no quotes here"), "no quotes here");
    }

    #[test]
    fn cleans_text() {
        assert_eq!(clean_text("a  \r\nb\r\n\r\n\r\n\r\nc\u{a0}\n"), "a\nb\n\nc");
    }

    #[test]
    fn inserts_above_quoted_original_and_escapes() {
        let html = insert_into_html_body(Some("<html><body lang=EN><p>From: Sara</p></body></html>"), "Hi <b>\n\nBye");
        let body_pos = html.find("<body lang=EN>").unwrap();
        let ours = html.find("Hi &lt;b&gt;").unwrap();
        assert!(body_pos < ours && ours < html.find("From: Sara").unwrap());
        assert!(html.contains(r#"dir="auto""#));
        assert!(insert_into_html_body(None, "a").starts_with("<div"));
        let s = BlockStyle { font_family: "Sakkal Majalla".into(), font_size_pt: 14.0, rtl: true };
        let h = insert_styled(Some("<html><body></body></html>"), "مرحبا", &s);
        assert!(h.contains(r#"<div dir="rtl" style="font-family:'Sakkal Majalla',Arial,sans-serif;font-size:14pt;text-align:right;">"#), "{h}");
    }

    #[test]
    fn html_to_text_keeps_words_and_lines() {
        let h = "<html><head><style>p{x}</style></head><body><p>Hello&nbsp;<b>Sara</b></p><div>سطر &#1575;&#x644;</div><!-- c --></body></html>";
        assert_eq!(html_to_text(h), "Hello Sara\nسطر ال");
        assert_eq!(body_or_html("  ", || "<p>x</p>".into()), "x");
        assert_eq!(body_or_html("plain", || unreachable!()), "plain");
    }

    #[test]
    fn truncates_by_chars() {
        assert_eq!(truncate_chars("السلام", 3), "الس");
    }
}
