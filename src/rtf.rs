//! Rich Text (RTF) bodies for the reading pane.
//!
//! Outlook items without `HTMLBody` (meeting invitations, some rich-text items) still carry `RTFBody`.
//! Most of these wrap the original HTML (`\fromhtml1`, [MS-OXRTFEX] 2.2.3.2): that HTML is recovered
//! exactly, tables and styles included. Plain RTF (and `\fromtext`) is converted to simple HTML:
//! paragraphs, bold/italic/underline/strike, sizes, colours, fonts, alignment, right-to-left, tables and
//! links. Pictures, objects and document properties are dropped.

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Tok<'a> {
    Open,
    Close,
    Word(&'a str, Option<i32>),
    Sym(u8),
    Hex(u8),
    Text(&'a [u8]),
}

struct Lexer<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Lexer<'a> {
    fn new(b: &'a [u8]) -> Self {
        Lexer { b, i: 0 }
    }
}

impl<'a> Iterator for Lexer<'a> {
    type Item = Tok<'a>;

    fn next(&mut self) -> Option<Tok<'a>> {
        let b = self.b;
        loop {
            let c = *b.get(self.i)?;
            match c {
                b'{' => {
                    self.i += 1;
                    return Some(Tok::Open);
                }
                b'}' => {
                    self.i += 1;
                    return Some(Tok::Close);
                }
                b'\r' | b'\n' | 0 => self.i += 1,
                b'\\' => {
                    self.i += 1;
                    let &n = b.get(self.i)?;
                    if n.is_ascii_alphabetic() {
                        let start = self.i;
                        while self.i < b.len() && b[self.i].is_ascii_alphabetic() && self.i - start < 32 {
                            self.i += 1;
                        }
                        let word = std::str::from_utf8(&b[start..self.i]).unwrap_or("");
                        let pstart = self.i;
                        if b.get(self.i) == Some(&b'-') {
                            self.i += 1;
                        }
                        let dstart = self.i;
                        while self.i < b.len() && b[self.i].is_ascii_digit() && self.i - dstart < 10 {
                            self.i += 1;
                        }
                        let param = if self.i > dstart {
                            std::str::from_utf8(&b[pstart..self.i]).ok().and_then(|p| p.parse::<i64>().ok())
                                .map(|v| v.clamp(i32::MIN as i64, i32::MAX as i64) as i32)
                        } else {
                            self.i = pstart; // a lone '-' is text, not part of the word
                            None
                        };
                        if b.get(self.i) == Some(&b' ') {
                            self.i += 1; // the delimiter space belongs to the control word
                        }
                        if word == "bin" {
                            self.i = (self.i + param.unwrap_or(0).max(0) as usize).min(b.len());
                            continue; // raw binary data (pictures)
                        }
                        return Some(Tok::Word(word, param));
                    }
                    self.i += 1;
                    match n {
                        b'\'' => {
                            let hex = b.get(self.i..self.i + 2).and_then(|h| std::str::from_utf8(h).ok())
                                .and_then(|h| u8::from_str_radix(h, 16).ok());
                            if let Some(v) = hex {
                                self.i += 2;
                                return Some(Tok::Hex(v));
                            }
                        }
                        b'\r' | b'\n' => return Some(Tok::Word("par", None)),
                        _ => return Some(Tok::Sym(n)),
                    }
                }
                _ => {
                    let start = self.i;
                    while self.i < b.len() && !matches!(b[self.i], b'{' | b'}' | b'\\' | b'\r' | b'\n' | 0) {
                        self.i += 1;
                    }
                    return Some(Tok::Text(&b[start..self.i]));
                }
            }
        }
    }
}

#[derive(Debug, PartialEq)]
enum Kind {
    Html,
    Text,
    Rtf,
}

/// [MS-OXRTFEX] 2.2.3.1: look for \fromhtml / \fromtext among the first 10 tokens.
fn kind(b: &[u8]) -> Option<Kind> {
    if !b.starts_with(b"{\\rtf") {
        return None;
    }
    for t in Lexer::new(b).take(10) {
        match t {
            Tok::Word("fromhtml", _) => return Some(Kind::Html),
            Tok::Word("fromtext", _) => return Some(Kind::Text),
            Tok::Open | Tok::Word(..) => {}
            _ => break,
        }
    }
    Some(Kind::Rtf)
}

/// HTML for an Outlook `RTFBody`; None if it isn't RTF or yields nothing.
pub fn to_html(rtf: &[u8]) -> Option<String> {
    let start = rtf.iter().position(|c| !c.is_ascii_whitespace()).unwrap_or(rtf.len());
    let rtf = &rtf[start..];
    let html = match kind(rtf)? {
        Kind::Html => deencapsulate(rtf),
        Kind::Text | Kind::Rtf => native(rtf),
    };
    (!html.trim().is_empty()).then_some(html)
}

// ------------------------------------------------------------------------------------------ code pages
/// Windows code page for an RTF \fcharset.
fn charset_cp(charset: i32) -> Option<u32> {
    Some(match charset {
        0 | 2 => 1252, // ANSI; Symbol is shown with Latin-1 glyphs
        77 => 10000,
        128 => 932,
        129 => 949,
        130 => 1361,
        134 => 936,
        136 => 950,
        161 => 1253,
        162 => 1254,
        163 => 1258,
        177 => 1255,
        178 => 1256,
        186 => 1257,
        204 => 1251,
        222 => 874,
        238 => 1250,
        254 => 437,
        255 => 850,
        _ => return None, // 1 = default
    })
}

/// Bytes in a Windows code page as text.
#[cfg(windows)]
pub fn decode(bytes: &[u8], cp: u32) -> String {
    use windows::Win32::Globalization::{MultiByteToWideChar, MULTI_BYTE_TO_WIDE_CHAR_FLAGS};
    if bytes.is_ascii() || cp == 65001 {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    unsafe {
        let n = MultiByteToWideChar(cp, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), bytes, None);
        if n <= 0 {
            return latin1(bytes);
        }
        let mut wide = vec![0u16; n as usize];
        let n = MultiByteToWideChar(cp, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), bytes, Some(&mut wide));
        String::from_utf16_lossy(&wide[..n.max(0) as usize])
    }
}

#[cfg(not(windows))]
pub fn decode(bytes: &[u8], cp: u32) -> String {
    if cp == 65001 { String::from_utf8_lossy(bytes).into_owned() } else { latin1(bytes) }
}

fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

/// Characters some control words stand for.
fn word_char(w: &str) -> Option<char> {
    Some(match w {
        "lquote" => '\u{2018}',
        "rquote" => '\u{2019}',
        "ldblquote" => '\u{201C}',
        "rdblquote" => '\u{201D}',
        "bullet" => '\u{2022}',
        "endash" => '\u{2013}',
        "emdash" => '\u{2014}',
        "emspace" => '\u{2003}',
        "enspace" => '\u{2002}',
        _ => return None,
    })
}

fn sym_char(c: u8) -> Option<char> {
    Some(match c {
        b'\\' | b'{' | b'}' => c as char,
        b'~' => '\u{A0}',
        b'_' => '\u{AD}',
        _ => return None, // \* \- \: \| ...
    })
}

fn unicode(p: Option<i32>) -> Option<char> {
    let v = p?;
    char::from_u32(if v < 0 { (v + 65536) as u32 } else { v as u32 })
}

/// Destinations that never produce visible text.
const SKIP_DESTS: &[&str] = &[
    "stylesheet", "info", "pict", "object", "objdata", "objclass", "header", "headerl", "headerr", "headerf", "footer",
    "footerl", "footerr", "footerf", "footnote", "listtable", "listoverridetable", "revtbl", "rsidtbl", "generator",
    "filetbl", "xmlnstbl", "themedata", "colorschememapping", "latentstyles", "datastore", "nonshppict", "shp",
    "shpinst", "shppict", "mmathPr", "pgdsctbl", "author", "operator", "title", "subject", "comment", "doccomm",
    "keywords", "company", "manager", "category", "template", "xe", "tc", "bkmkstart", "bkmkend", "atnid",
    "atnauthor", "annotation", "background", "ftnsep", "ftnsepc", "ftncn", "aftnsep", "aftnsepc", "aftncn",
    "defchp", "defpap", "pnseclvl", "pn", "pntxta", "pntxtb", "userprops", "docvar", "protusertbl", "falt",
];

#[derive(Debug, Clone, Copy, PartialEq)]
enum Dest {
    Body,
    Skip,
    HtmlTag,
    FontTbl,
    ColorTbl,
    FldInst,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct Fmt {
    b: bool,
    i: bool,
    u: bool,
    s: bool,
    /// Half-points; 0 = default.
    fs: i32,
    /// Colour table index; 0 = automatic.
    color: i32,
    bg: i32,
    font: i32,
}

#[derive(Debug, Clone)]
struct Group {
    dest: Dest,
    htmlrtf: bool,
    font: i32,
    uc: usize,
    fmt: Fmt,
    fresh: bool,
    starred: bool,
    /// This group (a \fldrslt) opened an <a>.
    link: bool,
}

impl Default for Group {
    fn default() -> Self {
        Group { dest: Dest::Body, htmlrtf: false, font: -1, uc: 1, fmt: Fmt { font: -1, ..Fmt::default() }, fresh: false,
                starred: false, link: false }
    }
}

/// Font table and colour table, shared by both readers.
#[derive(Default)]
struct Tables {
    ansi_cp: u32,
    default_font: i32,
    fonts: HashMap<i32, (String, Option<u32>)>,
    cur_font: i32,
    colors: Vec<Option<(u8, u8, u8)>>,
    cur_color: (Option<u8>, Option<u8>, Option<u8>),
}

impl Tables {
    fn new() -> Self {
        Tables { ansi_cp: 1252, default_font: -1, cur_font: -1, ..Default::default() }
    }

    /// The code page for text in `font`.
    fn cp(&self, font: i32) -> u32 {
        let f = if font < 0 { self.default_font } else { font };
        self.fonts.get(&f).and_then(|x| x.1).unwrap_or(self.ansi_cp)
    }

    fn header_word(&mut self, w: &str, p: Option<i32>) {
        match (w, p) {
            ("ansicpg", Some(cp)) if cp > 0 => self.ansi_cp = cp as u32,
            ("deff", Some(f)) => self.default_font = f,
            _ => {}
        }
    }

    fn font_word(&mut self, w: &str, p: Option<i32>) {
        match (w, p) {
            ("f", Some(n)) => {
                self.cur_font = n;
                self.fonts.entry(n).or_insert((String::new(), None));
            }
            ("fcharset", Some(c)) => {
                if let Some(f) = self.fonts.get_mut(&self.cur_font) {
                    if f.1.is_none() {
                        f.1 = charset_cp(c);
                    }
                }
            }
            ("cpg", Some(cp)) if cp > 0 => {
                if let Some(f) = self.fonts.get_mut(&self.cur_font) {
                    f.1 = Some(cp as u32);
                }
            }
            _ => {}
        }
    }

    fn font_text(&mut self, t: &[u8]) {
        if let Some(f) = self.fonts.get_mut(&self.cur_font) {
            if !f.0.ends_with(';') {
                f.0.push_str(&String::from_utf8_lossy(t));
            }
        }
    }

    fn color_word(&mut self, w: &str, p: Option<i32>) {
        let v = p.map(|v| v.clamp(0, 255) as u8);
        match w {
            "red" => self.cur_color.0 = v,
            "green" => self.cur_color.1 = v,
            "blue" => self.cur_color.2 = v,
            _ => {}
        }
    }

    fn color_text(&mut self, t: &[u8]) {
        for _ in t.iter().filter(|&&c| c == b';') {
            let (r, g, b) = self.cur_color;
            self.colors.push(if r.is_none() && g.is_none() && b.is_none() { None }
                             else { Some((r.unwrap_or(0), g.unwrap_or(0), b.unwrap_or(0))) });
            self.cur_color = (None, None, None);
        }
    }

    fn font_name(&self, font: i32) -> Option<&str> {
        let n = self.fonts.get(&font)?.0.trim_end_matches(';').trim();
        (!n.is_empty()).then_some(n)
    }

    fn color(&self, idx: i32) -> Option<String> {
        let (r, g, b) = (*self.colors.get(usize::try_from(idx).ok()?)?)?;
        Some(format!("#{r:02x}{g:02x}{b:02x}"))
    }
}

/// Decide a new group's destination from its first control word.
fn enter(g: &mut Group, w: &str, native: bool) {
    if !g.fresh {
        return;
    }
    g.fresh = false;
    if g.dest == Dest::Skip {
        return;
    }
    if g.starred {
        g.dest = match w {
            "htmltag" if !native => Dest::HtmlTag,
            "fldinst" if native => Dest::FldInst,
            _ => Dest::Skip, // ignorable destinations, incl. \*\mhtmltag
        };
        return;
    }
    match w {
        "fonttbl" => g.dest = Dest::FontTbl,
        "colortbl" => g.dest = Dest::ColorTbl,
        "fldinst" => g.dest = if native { Dest::FldInst } else { Dest::Skip },
        w if SKIP_DESTS.contains(&w) => g.dest = Dest::Skip,
        _ => {}
    }
}

/// Text (as bytes in one code page) waiting to be decoded together, so double-byte characters split
/// across \'hh escapes survive.
#[derive(Default)]
struct Pending {
    bytes: Vec<u8>,
    cp: u32,
}

impl Pending {
    fn push(&mut self, b: &[u8], cp: u32, out: &mut impl FnMut(&str)) {
        if cp != self.cp {
            self.flush(out);
            self.cp = cp;
        }
        self.bytes.extend_from_slice(b);
    }

    fn flush(&mut self, out: &mut impl FnMut(&str)) {
        if !self.bytes.is_empty() {
            out(&decode(&self.bytes, self.cp));
            self.bytes.clear();
        }
    }
}

/// Bytes still to drop after a \uN (its ANSI fallback); returns the part of `t` to keep.
fn after_skip<'a>(skip: &mut usize, t: &'a [u8]) -> &'a [u8] {
    let n = (*skip).min(t.len());
    *skip -= n;
    &t[n..]
}

// ------------------------------------------------------------------------------------------ \fromhtml
/// Recover the HTML that Outlook wrapped in RTF ([MS-OXRTFEX] 2.2.3.2).
fn deencapsulate(b: &[u8]) -> String {
    let mut tables = Tables::new();
    let mut stack = vec![Group::default()];
    let mut html = String::new();
    let mut pending = Pending::default();
    let mut skip = 0usize;
    for tok in Lexer::new(b) {
        if !matches!(tok, Tok::Text(_) | Tok::Hex(_)) {
            pending.flush(&mut |s| html.push_str(s));
        }
        let g = stack.last_mut().expect("root group");
        match tok {
            Tok::Open => {
                let mut child = g.clone();
                child.fresh = true;
                child.starred = false;
                stack.push(child);
                skip = 0;
            }
            Tok::Close => {
                if stack.len() > 1 {
                    stack.pop();
                }
                skip = 0;
            }
            Tok::Sym(b'*') if g.fresh => g.starred = true,
            Tok::Word(w, p) => {
                enter(g, w, false);
                if stack.len() <= 2 {
                    tables.header_word(w, p);
                }
                let g = stack.last_mut().expect("root group");
                match g.dest {
                    Dest::Skip | Dest::ColorTbl | Dest::FldInst => {}
                    Dest::FontTbl => tables.font_word(w, p),
                    Dest::HtmlTag => match w {
                        "par" => html.push_str("\r\n"),
                        "tab" => html.push('\t'),
                        "uc" => g.uc = p.unwrap_or(1).max(0) as usize,
                        "u" => {
                            if let Some(c) = unicode(p) {
                                html.push(c);
                            }
                            skip = g.uc;
                        }
                        w => {
                            if let Some(c) = word_char(w) {
                                html.push(c);
                            }
                        }
                    },
                    Dest::Body => match w {
                        "f" => g.font = p.unwrap_or(-1), // tracked even while suppressed
                        "htmlrtf" => g.htmlrtf = p != Some(0),
                        "uc" => g.uc = p.unwrap_or(1).max(0) as usize,
                        _ if g.htmlrtf => {}
                        "par" | "line" => html.push_str("\r\n"),
                        "tab" => html.push('\t'),
                        "u" => {
                            if let Some(c) = unicode(p) {
                                html.push(c);
                            }
                            skip = g.uc;
                        }
                        w => {
                            if let Some(c) = word_char(w) {
                                html.push(c);
                            }
                        }
                    },
                }
            }
            Tok::Sym(c) => {
                g.fresh = false;
                let emit = match g.dest {
                    Dest::HtmlTag => true,
                    Dest::Body => !g.htmlrtf,
                    _ => false,
                };
                if emit {
                    if let Some(ch) = sym_char(c) {
                        html.push(ch);
                    }
                }
            }
            Tok::Hex(v) => {
                g.fresh = false;
                if after_skip(&mut skip, &[v]).is_empty() {
                    continue;
                }
                match g.dest {
                    Dest::HtmlTag => pending.push(&[v], tables.ansi_cp, &mut |s| html.push_str(s)),
                    Dest::Body if !g.htmlrtf => pending.push(&[v], tables.cp(g.font), &mut |s| html.push_str(s)),
                    Dest::FontTbl => tables.font_text(&[v]),
                    _ => {}
                }
            }
            Tok::Text(t) => {
                g.fresh = false;
                let t = after_skip(&mut skip, t);
                if t.is_empty() {
                    continue;
                }
                match g.dest {
                    Dest::HtmlTag => pending.push(t, tables.ansi_cp, &mut |s| html.push_str(s)),
                    Dest::Body if !g.htmlrtf => pending.push(t, tables.cp(g.font), &mut |s| html.push_str(s)),
                    Dest::FontTbl => tables.font_text(t),
                    _ => {}
                }
            }
        }
    }
    pending.flush(&mut |s| html.push_str(s));
    html
}

// ------------------------------------------------------------------------------------------ plain RTF
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Para {
    /// 0 left, 1 centre, 2 right, 3 justified
    align: u8,
    rtl: bool,
    intbl: bool,
}

/// HTML being built from plain RTF: paragraphs and table cells opened lazily, one <span> per run.
#[derive(Default)]
struct Writer {
    out: String,
    in_p: bool,
    table: bool,
    row: bool,
    cell: bool,
    /// The run format in effect, and whether a <span> was opened for it.
    span: Option<(Fmt, bool)>,
}

fn esc(s: &str) -> String {
    crate::text::escape_html(s)
}

impl Writer {
    fn close_span(&mut self) {
        if let Some((_, true)) = self.span.take() {
            self.out.push_str("</span>");
        }
    }

    fn close_table(&mut self) {
        self.close_span();
        if self.cell {
            self.out.push_str("</td>");
            self.cell = false;
        }
        if self.row {
            self.out.push_str("</tr>");
            self.row = false;
        }
        if self.table {
            self.out.push_str("</table>");
            self.table = false;
        }
    }

    fn open_cell(&mut self) {
        if self.in_p {
            self.close_span();
            self.out.push_str("</p>");
            self.in_p = false;
        }
        if !self.table {
            self.out.push_str("<table style=\"border-collapse:collapse;margin:4px 0\">");
            self.table = true;
        }
        if !self.row {
            self.out.push_str("<tr>");
            self.row = true;
        }
        if !self.cell {
            self.out.push_str("<td style=\"border:1px solid #a6a6a6;padding:2px 6px;vertical-align:top\">");
            self.cell = true;
        }
    }

    fn block(&mut self, para: &Para) {
        if para.intbl {
            self.open_cell();
            return;
        }
        if self.table {
            self.close_table();
        }
        if !self.in_p {
            let align = ["", "center", "right", "justify"][para.align.min(3) as usize];
            self.out.push_str(&format!("<p style=\"margin:0 0 .35em{}\"{}>",
                if align.is_empty() { String::new() } else { format!(";text-align:{align}") },
                if para.rtl { " dir=\"rtl\"" } else { "" }));
            self.in_p = true;
        }
    }

    fn text(&mut self, s: &str, fmt: &Fmt, para: &Para, t: &Tables) {
        if s.is_empty() {
            return;
        }
        self.block(para);
        if self.span.map(|s| s.0) != Some(*fmt) {
            self.close_span();
            let mut css = Vec::new();
            if fmt.b { css.push("font-weight:bold".to_string()); }
            if fmt.i { css.push("font-style:italic".into()); }
            match (fmt.u, fmt.s) {
                (true, true) => css.push("text-decoration:underline line-through".into()),
                (true, false) => css.push("text-decoration:underline".into()),
                (false, true) => css.push("text-decoration:line-through".into()),
                _ => {}
            }
            if fmt.fs > 0 { css.push(format!("font-size:{}pt", fmt.fs as f32 / 2.0)); }
            if let Some(c) = t.color(fmt.color) { css.push(format!("color:{c}")); }
            if let Some(c) = t.color(fmt.bg) { css.push(format!("background:{c}")); }
            if let Some(f) = t.font_name(fmt.font) {
                let f: String = f.chars().filter(|c| !"'\";<>{}".contains(*c)).collect();
                css.push(format!("font-family:'{f}'"));
            }
            // Every value is generated here; font names are stripped of quotes and brackets.
            let open = !css.is_empty();
            if open {
                self.out.push_str(&format!("<span style=\"{}\">", css.join(";")));
            }
            self.span = Some((*fmt, open));
        }
        self.out.push_str(&esc(s));
    }

    fn par(&mut self, para: &Para) {
        self.close_span();
        if self.cell {
            self.out.push_str("<br>");
        } else if self.in_p {
            self.out.push_str("</p>");
            self.in_p = false;
        } else if !para.intbl {
            if self.table {
                self.close_table();
            }
            self.out.push_str("<p style=\"margin:0 0 .35em\">&nbsp;</p>");
        }
    }

    fn raw(&mut self, html: &str, para: &Para) {
        self.block(para);
        self.close_span();
        self.out.push_str(html);
    }

    fn finish(mut self) -> String {
        self.close_table();
        self.close_span();
        if self.in_p {
            self.out.push_str("</p>");
        }
        self.out
    }
}

/// The URL of a HYPERLINK field instruction (web and mail links only).
fn hyperlink(inst: &str) -> Option<String> {
    let rest = inst.trim().strip_prefix("HYPERLINK")?.trim();
    if rest.starts_with("\\l") {
        return None; // a link to a bookmark in the same document
    }
    let url = match rest.strip_prefix('"') {
        Some(r) => r.split('"').next()?,
        None => rest.split_whitespace().next()?,
    };
    let lower = url.to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:")).then(|| url.to_string())
}

/// Convert plain RTF (also \fromtext) to simple HTML.
fn native(b: &[u8]) -> String {
    let mut tables = Tables::new();
    let mut stack = vec![Group::default()];
    let mut w = Writer::default();
    let mut para = Para::default();
    let mut pending = Pending::default();
    let mut skip = 0usize;
    let mut fld_inst = String::new();
    for tok in Lexer::new(b) {
        if !matches!(tok, Tok::Text(_) | Tok::Hex(_)) && !pending.bytes.is_empty() {
            let g = stack.last().expect("root group");
            let (fmt, dest) = (g.fmt, g.dest);
            pending.flush(&mut |s| match dest {
                Dest::FldInst => fld_inst.push_str(s),
                _ => w.text(s, &fmt, &para, &tables),
            });
        }
        let g = stack.last_mut().expect("root group");
        match tok {
            Tok::Open => {
                let mut child = g.clone();
                child.fresh = true;
                child.starred = false;
                child.link = false;
                stack.push(child);
                skip = 0;
            }
            Tok::Close => {
                if stack.len() > 1 && stack.pop().is_some_and(|g| g.link) {
                    w.close_span();
                    w.out.push_str("</a>");
                }
                skip = 0;
            }
            Tok::Sym(b'*') if g.fresh => g.starred = true,
            Tok::Word(word, p) => {
                enter(g, word, true);
                if stack.len() <= 2 {
                    tables.header_word(word, p);
                }
                let g = stack.last_mut().expect("root group");
                match g.dest {
                    Dest::Skip | Dest::HtmlTag => continue,
                    Dest::FontTbl => {
                        tables.font_word(word, p);
                        continue;
                    }
                    Dest::ColorTbl => {
                        tables.color_word(word, p);
                        continue;
                    }
                    Dest::FldInst | Dest::Body => {}
                }
                let on = p != Some(0);
                match word {
                    "field" => fld_inst.clear(),
                    "fldrslt" => {
                        if let Some(url) = hyperlink(&fld_inst) {
                            w.raw(&format!("<a href=\"{}\">", esc(&url)), &para);
                            g.link = true;
                        }
                        fld_inst.clear();
                    }
                    "uc" => g.uc = p.unwrap_or(1).max(0) as usize,
                    "u" => {
                        if let Some(c) = unicode(p) {
                            let fmt = g.fmt;
                            if g.dest == Dest::FldInst { fld_inst.push(c) } else { w.text(&c.to_string(), &fmt, &para, &tables) }
                        }
                        skip = g.uc;
                    }
                    "f" => {
                        g.font = p.unwrap_or(-1);
                        g.fmt.font = g.font;
                    }
                    "plain" => g.fmt = Fmt { font: g.font, ..Fmt::default() },
                    "b" => g.fmt.b = on,
                    "i" => g.fmt.i = on,
                    "ul" | "uld" | "uldash" | "uldb" | "ulth" | "ulw" | "ulwave" => g.fmt.u = on,
                    "ulnone" => g.fmt.u = false,
                    "strike" | "striked" => g.fmt.s = on,
                    "fs" => g.fmt.fs = p.unwrap_or(0).clamp(0, 400),
                    "cf" => g.fmt.color = p.unwrap_or(0),
                    "highlight" | "cb" | "chcbpat" => g.fmt.bg = p.unwrap_or(0),
                    "pard" => para = Para::default(),
                    "intbl" => para.intbl = true,
                    "ql" => para.align = 0,
                    "qc" => para.align = 1,
                    "qr" => para.align = 2,
                    "qj" => para.align = 3,
                    "rtlpar" => para.rtl = true,
                    "ltrpar" => para.rtl = false,
                    "par" | "sect" | "page" if g.dest == Dest::Body => w.par(&para),
                    "line" if g.dest == Dest::Body => w.raw("<br>", &para),
                    "tab" if g.dest == Dest::Body => w.text("\u{2003}", &g.fmt.clone(), &para, &tables),
                    "cell" | "nestcell" if g.dest == Dest::Body => {
                        w.open_cell();
                        w.close_span();
                        w.out.push_str("</td>");
                        w.cell = false;
                    }
                    "row" | "nestrow" if g.dest == Dest::Body => {
                        w.close_span();
                        if w.cell {
                            w.out.push_str("</td>");
                            w.cell = false;
                        }
                        if w.row {
                            w.out.push_str("</tr>");
                            w.row = false;
                        }
                    }
                    other => {
                        if let Some(c) = word_char(other) {
                            let fmt = g.fmt;
                            if g.dest == Dest::FldInst { fld_inst.push(c) } else { w.text(&c.to_string(), &fmt, &para, &tables) }
                        }
                    }
                }
            }
            Tok::Sym(c) => {
                g.fresh = false;
                if let Some(ch) = sym_char(c) {
                    match g.dest {
                        Dest::Body => {
                            let fmt = g.fmt;
                            w.text(&ch.to_string(), &fmt, &para, &tables);
                        }
                        Dest::FldInst => fld_inst.push(ch),
                        _ => {}
                    }
                }
            }
            Tok::Hex(v) => {
                g.fresh = false;
                if after_skip(&mut skip, &[v]).is_empty() {
                    continue;
                }
                match g.dest {
                    Dest::Body | Dest::FldInst => {
                        let (fmt, dest, cp) = (g.fmt, g.dest, tables.cp(g.font));
                        pending.push(&[v], cp, &mut |s| match dest {
                            Dest::FldInst => fld_inst.push_str(s),
                            _ => w.text(s, &fmt, &para, &tables),
                        });
                    }
                    Dest::FontTbl => tables.font_text(&[v]),
                    _ => {}
                }
            }
            Tok::Text(t) => {
                g.fresh = false;
                let t = after_skip(&mut skip, t);
                if t.is_empty() {
                    continue;
                }
                match g.dest {
                    Dest::Body | Dest::FldInst => {
                        let (fmt, dest, cp) = (g.fmt, g.dest, tables.cp(g.font));
                        pending.push(t, cp, &mut |s| match dest {
                            Dest::FldInst => fld_inst.push_str(s),
                            _ => w.text(s, &fmt, &para, &tables),
                        });
                    }
                    Dest::FontTbl => tables.font_text(t),
                    Dest::ColorTbl => tables.color_text(t),
                    _ => {}
                }
            }
        }
    }
    let fmt = stack.last().map(|g| g.fmt).unwrap_or_default();
    pending.flush(&mut |s| w.text(s, &fmt, &para, &tables));
    let body = w.finish();
    if body.trim().is_empty() {
        return String::new();
    }
    let font = tables.font_name(tables.default_font).map(|f| f.chars().filter(|c| !"'\";<>{}".contains(*c)).collect::<String>());
    format!("<html><head><meta charset=\"utf-8\"></head><body><div style=\"font-family:{}Calibri,Arial,sans-serif;font-size:11pt\">{body}</div></body></html>",
            font.map(|f| format!("'{f}',")).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovers_encapsulated_html_exactly() {
        // As Outlook writes meeting invitations: HTML in \*\htmltag groups, RTF-only parts behind
        // \htmlrtf, Arabic text in a \fcharset178 (code page 1256) font, \u escapes, a CID picture.
        let rtf = br#"{\rtf1\ansi\ansicpg1252\fromhtml1 \fbidis \deff0{\fonttbl
{\f0\fswiss Arial;}{\f1\fswiss\fcharset178 Arial;}{\f2\fnil\fcharset2 Symbol;}}
{\colortbl\red0\green0\blue0;\red5\green99\blue193;}
{\*\htmltag19 <html>}{\*\htmltag34 <head>}{\*\htmltag241 <style>}{\*\htmltag241 p \{margin:0\}\par td \{border:1px solid\}}
{\*\htmltag249 </style>}{\*\htmltag41 </head>}
{\*\htmltag50 <body>}\htmlrtf {\htmlrtf0 {\*\htmltag64 <p dir=rtl>}\htmlrtf {\htmlrtf0 \f1 \'e3\'d1\'cd\'c8\'c7 caf\u233?\htmlrtf\par}\htmlrtf0
{\*\htmltag72 </p>}{\*\htmltag84 <table class=MsoTableGrid>}{\*\htmltag84 <tr>}{\*\htmltag84 <td>}\htmlrtf {\htmlrtf0 \f0 A\'26B\htmlrtf\cell}\htmlrtf0
{\*\htmltag84 </td></tr></table>}{\*\mhtmltag84 <img src="cid:x@y">}{\*\htmltag84 <img src="cid:image001.png">}
\htmlrtf\f2 \'b7\tab\htmlrtf0 {\*\htmltag244 <o:p>}{\*\htmltag84 &nbsp;}\htmlrtf }\htmlrtf0 {\*\htmltag58 </body>}{\*\htmltag27 </html>}}"#;
        assert_eq!(kind(rtf), Some(Kind::Html));
        let html = to_html(rtf).unwrap();
        assert!(html.starts_with("<html><head><style>p {margin:0}\r\ntd {border:1px solid}</style></head>"), "{html}");
        assert!(html.contains("<p dir=rtl>مرحبا café</p>"), "code page 1256 + \\u escape, suppressed \\par dropped: {html}");
        assert!(html.contains("<table class=MsoTableGrid><tr><td>A&B</td></tr></table>"), "{html}");
        assert!(html.contains("<img src=\"cid:image001.png\">") && !html.contains("cid:x@y"), "mhtmltag skipped");
        assert!(!html.contains("Arial") && !html.contains('·'), "font table and suppressed RTF text left out");
        assert!(html.ends_with("&nbsp;</body></html>"), "{html}");
    }

    #[test]
    fn converts_plain_rtf() {
        let rtf = br#"{\rtf1\ansi\ansicpg1252\deff0{\fonttbl{\f0\fswiss\fcharset0 Calibri;}{\f1\fnil\fcharset178 Sakkal Majalla;}}
{\colortbl;\red192\green0\blue0;}{\*\generator Riched20;}{\info{\author Someone}}
\pard\plain\f0\fs22 Hello {\b bold} and {\cf1 red} <x> & co.\par
\pard\rtlpar\qr\f1\fs28 \'e3\'d1\'cd\'c8\'c7\par
\trowd\cellx2000\cellx4000\pard\intbl\f0 A1\cell B1\cell\row
\trowd\cellx2000\cellx4000\pard\intbl A2\cell B2\cell\row
\pard {\field{\*\fldinst{HYPERLINK "https://example.org/x"}}{\fldrslt{\ul link}}} end\par
{\pict\pngblip\picw10 89504e470d0a}{\field{\*\fldinst HYPERLINK "javascript:alert(1)"}{\fldrslt bad}}
}"#;
        assert_eq!(kind(rtf), Some(Kind::Rtf));
        let html = to_html(rtf).unwrap();
        assert!(html.contains("font-family:'Calibri',Calibri"), "default font: {html}");
        assert!(html.contains("<span style=\"font-weight:bold;font-size:11pt;font-family:'Calibri'\">bold</span>"), "{html}");
        assert!(html.contains("color:#c00000") && html.contains(">red</span>"), "{html}");
        assert!(html.contains("&lt;x&gt; &amp; co."), "text escaped: {html}");
        assert!(html.contains("dir=\"rtl\"") && html.contains("text-align:right") && html.contains("مرحبا")
                && html.contains("font-family:'Sakkal Majalla'"), "{html}");
        assert_eq!(html.matches("<tr>").count(), 2, "{html}");
        assert!(html.contains(">A1</span></td>") && html.contains("B2"), "{html}");
        assert!(html.contains("<a href=\"https://example.org/x\">") && html.contains(">link</span></a>"), "{html}");
        assert!(!html.contains("javascript") && html.contains("bad"), "only web/mail links become links");
        assert!(!html.contains("89504e47") && !html.contains("Riched20") && !html.contains("Someone"), "pictures and properties dropped");
    }

    #[test]
    fn plain_text_rtf_and_non_rtf() {
        let html = to_html(br"{\rtf1\ansi\fromtext \fbidis \deff0{\fonttbl{\f0\fmodern Courier New;}}\f0 line one\par line {two}\par}").unwrap();
        assert!(html.contains(">line one</span></p>") && html.contains("line two"), "{html}");
        assert_eq!(to_html(b"not rtf"), None);
        assert_eq!(to_html(b""), None);
        assert_eq!(to_html(br"{\rtf1\ansi{\fonttbl{\f0 Arial;}}}"), None, "nothing visible");
    }
}
