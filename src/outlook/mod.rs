//! Classic Outlook through COM automation.
//!
//! * Outlook's objects are apartment-threaded, so every call runs on ONE dedicated thread that
//!   called CoInitializeEx(STA). Other threads send it closures and wait for the result. COM objects
//!   never leave that thread; only plain data (and EntryIDs) do.
//! * Nothing here can send mail: writes only `.Save()` to Drafts, and `Disp` refuses `Send`.
//! * Date filtering happens in Rust, not with Items.Restrict(): Restrict date strings depend on the
//!   Windows locale and break on Arabic Windows.
//! * Reading `Body` does not mark mail as read; nothing here changes `UnRead`.

mod dispatch;

pub use dispatch::{ole_date, Disp, Var};

use crate::mail::*;
use crate::text::{body_or_html, clean_text, strip_quoted, truncate_chars};

/// Plain-text body, falling back to the HTML body (HTML-only or rights-protected mail has an empty `Body`).
fn body_text(item: &Disp) -> String {
    body_or_html(&item.str_or_empty("Body"), || item.str_or_empty("HTMLBody"))
}
use chrono::{Duration, Local, NaiveDateTime};
use dispatch::ComError;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use windows::core::w;
use windows::Win32::System::Com::{CLSIDFromProgID, CoCreateInstance, CoInitializeEx, IDispatch, CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED};

const OL_MAIL_ITEM: i64 = 0;
const OL_TO: i64 = 1;
const OL_CC: i64 = 2;
const OL_CLASS_MAIL: i64 = 43;
const OL_MEETING_CLASSES: [i64; 5] = [53, 54, 55, 56, 57];
const OL_FOLDER_INBOX: i64 = 6;
const OL_FOLDER_DRAFTS: i64 = 16;
const PR_SMTP_ADDRESS: &str = "http://schemas.microsoft.com/mapi/proptag/0x39FE001E";
const PR_ATTACHMENT_HIDDEN: &str = "http://schemas.microsoft.com/mapi/proptag/0x7FFE000B";
/// PR_LAST_VERB_EXECUTED / PR_LAST_VERB_EXECUTION_TIME: Outlook's "You replied/forwarded on …".
const PR_LAST_VERB: &str = "http://schemas.microsoft.com/mapi/proptag/0x10810003";
const PR_LAST_VERB_TIME: &str = "http://schemas.microsoft.com/mapi/proptag/0x10820040";

/// Outlook's last action on an item, as (name, local time).
fn last_action(item: &Disp) -> (Option<String>, Option<String>) {
    let Ok(pa) = item.get_obj("PropertyAccessor") else { return (None, None) };
    let action = pa.call("GetProperty", &[Var::Str(PR_LAST_VERB.into())]).ok()
        .and_then(|v| v.as_i64()).and_then(verb_name).map(String::from);
    if action.is_none() {
        return (None, None);
    }
    let at = pa.call("GetProperty", &[Var::Str(PR_LAST_VERB_TIME.into())]).ok()
        .and_then(|utc| pa.call("UTCToLocalTime", &[utc]).ok())
        .and_then(|v| match v { Var::Date(d) | Var::Float(d) => iso(ole_date(d)), _ => None });
    (action, at)
}

fn default_folder(name: &str) -> Option<i64> {
    Some(match name {
        "inbox" => 6,
        "drafts" => 16,
        "sent" | "sent items" => 5,
        "deleted" | "deleted items" => 3,
        "outbox" => 4,
        "junk" | "junk email" => 23,
        _ => return None,
    })
}

impl From<ComError> for MailError {
    fn from(e: ComError) -> Self {
        MailError::Outlook(e.0)
    }
}

type Job = Box<dyn FnOnce(&mut Conn) + Send>;

struct Conn {
    ns: Option<Disp>,
    /// When we last started Outlook ourselves (to avoid relaunch loops).
    launched_at: Option<std::time::Instant>,
}

/// Is classic Outlook (OUTLOOK.EXE) running for this user?
pub fn outlook_running() -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return false };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut found = false;
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
                if String::from_utf16_lossy(&e.szExeFile[..len]).eq_ignore_ascii_case("outlook.exe") {
                    found = true;
                    break;
                }
                if Process32NextW(snap, &mut e).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
        found
    }
}

/// Start classic Outlook as a normal (minimized, not focused) window. `/recycle` reuses an existing
/// window instead of opening a second one.
fn launch_outlook() -> Result<(), String> {
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWMINNOACTIVE;
    let r = unsafe { ShellExecuteW(None, w!("open"), w!("outlook.exe"), w!("/recycle"), PCWSTR::null(), SW_SHOWMINNOACTIVE) };
    if r.0 as isize <= 32 {
        return Err(format!("Windows could not start classic Outlook (code {})", r.0 as isize));
    }
    Ok(())
}

fn connect() -> Result<Disp, String> {
    let clsid = unsafe { CLSIDFromProgID(w!("Outlook.Application")) }
        .map_err(|e| format!("Outlook is not installed ({})", e.message()))?;
    let app: IDispatch = unsafe { CoCreateInstance(&clsid, None, CLSCTX_LOCAL_SERVER) }
        .map_err(|e| e.message().to_string())?;
    let ns = Disp(app).call_obj("GetNamespace", &[Var::Str("MAPI".into())]).map_err(|e| e.0)?;
    ns.call_obj("GetDefaultFolder", &[Var::Int(OL_FOLDER_INBOX)]).map_err(|e| e.0)?; // ready to use?
    Ok(ns)
}

impl Conn {
    fn namespace(&mut self) -> MailResult<Disp> {
        if let Some(ns) = &self.ns {
            return Ok(ns.clone());
        }
        // Not running: start classic Outlook visibly (not as a hidden automation server), then wait
        // until it answers. At most once every 2 minutes, so a failing start can't loop.
        if !outlook_running() && self.launched_at.is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(120)) {
            self.launched_at = Some(std::time::Instant::now());
            launch_outlook().map_err(MailError::Outlook)?;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
            let mut last = String::from("Outlook is still starting");
            while std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_secs(2));
                if outlook_running() {
                    match connect() {
                        Ok(ns) => {
                            self.ns = Some(ns.clone());
                            return Ok(ns);
                        }
                        Err(e) => last = e, // e.g. "call was rejected" while Outlook loads the profile
                    }
                }
            }
            return Err(MailError::Outlook(format!(
                "Classic Outlook was started but isn't ready yet ({last}). Wait until it has opened, then click ⟳.")));
        }
        let ns = connect().map_err(|e| MailError::Outlook(format!(
            "Could not connect to Outlook. Make sure classic Outlook is running under the same Windows user. ({e})")))?;
        self.ns = Some(ns.clone());
        Ok(ns)
    }
}

pub struct OutlookMail {
    tx: mpsc::Sender<Job>,
}

impl OutlookMail {
    /// Start the COM thread. Connecting to Outlook happens lazily on the first call.
    pub fn start() -> Self {
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("outlook-com".into())
            .spawn(move || {
                let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
                let mut conn = Conn { ns: None, launched_at: None };
                for job in rx {
                    job(&mut conn);
                }
            })
            .expect("spawn Outlook COM thread");
        OutlookMail { tx }
    }

    /// Self-test only: Outlook's `BodyFormat` (1 plain, 2 HTML, 3 rich text) and message class of an item.
    /// Read-only.
    pub fn body_format(&self, entry_id: &str) -> MailResult<(i64, String, String)> {
        let entry_id = entry_id.to_string();
        self.run(true, move |ns| {
            let item = get_item(ns, &entry_id)?;
            // What a body-less item carries instead: RTF (native / wrapping HTML / wrapping text) or PR_HTML.
            let rtf = match item.get("RTFBody") { Ok(Var::Bytes(b)) => b, _ => Vec::new() };
            let head = String::from_utf8_lossy(&rtf[..rtf.len().min(400)]).to_string();
            let rtf_kind = if rtf.is_empty() { "no-rtf" } else if head.contains("\\fromhtml") { "rtf-html" }
                           else if head.contains("\\fromtext") { "rtf-text" } else { "rtf-native" };
            let pr_html = item.get_obj("PropertyAccessor")
                .and_then(|pa| pa.call("GetProperty", &[Var::Str("http://schemas.microsoft.com/mapi/proptag/0x10130102".into())]))
                .map(|v| matches!(v, Var::Bytes(b) if !b.is_empty())).unwrap_or(false);
            let extra = format!("{rtf_kind}{}", if pr_html { "+pr_html" } else { "" });
            Ok((item.i64_or("BodyFormat", 0), item.str_or_empty("MessageClass"), extra))
        })
    }

    /// Self-test only: build a forward of `entry_id` in memory, write `text` into it exactly as a real
    /// forward does, report (method, text landed?, original item class), then DISCARD it.
    /// Never saved, never sent: nothing appears in Drafts.
    pub fn dry_run_forward(&self, entry_id: &str, text: &str, style: crate::text::BlockStyle) -> MailResult<(String, bool, i64)> {
        let (entry_id, text) = (entry_id.to_string(), text.to_string());
        self.run(false, move |ns| {
            let item = get_item(ns, &entry_id)?;
            let class = item.i64_or("Class", -1);
            let (fwd, meeting_details) = build_forward(ns, &item, &style, crate::text::has_arabic(&text))?;
            let w = match &meeting_details {
                Some(d) => {
                    fwd.put("HTMLBody", Var::Str(crate::text::insert_styled(Some(&format!("<html><body>{d}</body></html>")), &text, &style)))?;
                    Written::Html
                }
                None => write_message(&fwd, &text, &style)?,
            };
            let first = text.lines().next().unwrap_or("").to_string();
            let attached = if meeting_details.is_some() { format!("email + {} attachment(s)", attachment_count(&fwd)) } else { "html".into() };
            let (how, landed) = match &w {
                Written::Html => (attached.as_str(), fwd.str_or_empty("HTMLBody").contains(&crate::text::escape_html(&first))),
                Written::Word(insp) => ("word", insp.get_obj("WordEditor").and_then(|d| d.get_obj("Content"))
                    .map(|c| c.str_or_empty("Text").contains(&first)).unwrap_or(false)),
                Written::Plain => ("plain", fwd.str_or_empty("Body").contains(&first)),
            };
            if let Written::Word(insp) = &w {
                let _ = insp.call("Close", &[Var::Int(1)]); // olDiscard
            }
            Ok((how.to_string(), landed, class))
        })
    }

    /// Run `op` on the COM thread. With `retry`, a COM failure (e.g. Outlook was restarted and the
    /// proxy is stale) reconnects once. Writes pass `retry = false` so a retry can't duplicate drafts.
    fn run<T: Send + 'static>(&self, retry: bool, op: impl Fn(&Disp) -> MailResult<T> + Send + 'static) -> MailResult<T> {
        let (rtx, rrx) = mpsc::channel();
        let job: Job = Box::new(move |conn: &mut Conn| {
            let result = (|| {
                let ns = conn.namespace()?;
                match op(&ns) {
                    Err(MailError::Outlook(_)) if retry => {
                        conn.ns = None;
                        let ns = conn.namespace()?;
                        op(&ns)
                    }
                    other => other,
                }
            })();
            let _ = rtx.send(result);
        });
        self.tx.send(job).map_err(|_| MailError::Outlook("The Outlook thread stopped.".into()))?;
        rrx.recv().map_err(|_| MailError::Outlook("The Outlook thread stopped.".into()))?
    }
}

// ---------------------------------------------------------------------------------- helpers
fn iso(d: Option<NaiveDateTime>) -> Option<String> {
    use chrono::Datelike;
    d.filter(|d| d.year() < 4000) // Outlook uses 4501-01-01 for "none"
        .map(|d| d.format("%Y-%m-%dT%H:%M").to_string())
}

fn is_supported(item: &Disp) -> bool {
    let cls = item.i64_or("Class", -1);
    cls == OL_CLASS_MAIL || OL_MEETING_CLASSES.contains(&cls)
}

/// Exchange senders come back as X.500 DNs; resolve to the SMTP address.
fn sender_smtp(item: &Disp) -> String {
    if item.str_or_empty("SenderEmailType") == "EX" {
        if let Ok(sender) = item.get_obj("Sender") {
            if let Ok(ex) = sender.call_obj("GetExchangeUser", &[]) {
                let smtp = ex.str_or_empty("PrimarySmtpAddress");
                if !smtp.is_empty() {
                    return smtp;
                }
            }
            if let Ok(pa) = sender.get_obj("PropertyAccessor") {
                if let Ok(v) = pa.call("GetProperty", &[Var::Str(PR_SMTP_ADDRESS.into())]) {
                    let s = v.into_string();
                    if !s.is_empty() {
                        return s;
                    }
                }
            }
        }
    }
    item.str_or_empty("SenderEmailAddress")
}

fn importance(item: &Disp) -> String {
    match item.i64_or("Importance", 1) {
        0 => "low",
        2 => "high",
        _ => "normal",
    }.into()
}

fn attachment_count(item: &Disp) -> i64 {
    item.get_obj("Attachments").map(|a| a.i64_or("Count", 0)).unwrap_or(0)
}

fn summary(item: &Disp) -> Summary {
    let body = body_text(item);
    let subject = item.str_or_empty("Subject");
    Summary {
        entry_id: item.str_or_empty("EntryID"),
        kind: if item.i64_or("Class", -1) == OL_CLASS_MAIL { "email" } else { "meeting" }.into(),
        subject: if subject.is_empty() { "(no subject)".into() } else { subject },
        from_name: item.str_or_empty("SenderName"),
        from: sender_smtp(item),
        received: iso(item.date("ReceivedTime")),
        unread: item.bool_or_false("UnRead"),
        importance: importance(item),
        attachments: attachment_count(item),
        preview: truncate_chars(&strip_quoted(&body), PREVIEW_CHARS),
        folder: None,
        tags: vec![],
        to: item.str_or_empty("To"),
        cc: item.str_or_empty("CC"),
        body_head: truncate_chars(&body, 4000),
    }
}

/// Display names of a draft's To recipients (after Reply/ReplyAll/Forward built them).
fn to_names(mail: &Disp) -> Vec<String> {
    mail.get_obj("Recipients").map(|r| r.items().iter()
        .filter(|x| x.i64_or("Type", OL_TO) == OL_TO)
        .map(|x| x.str_or_empty("Name")).filter(|n| !n.is_empty()).collect()).unwrap_or_default()
}

fn find_folder(ns: &Disp, path: &str) -> MailResult<Disp> {
    let trimmed = path.trim().trim_matches(['/', '\\']);
    let mut parts: Vec<&str> = trimmed.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        parts.push("inbox");
    }
    let (mut folder, rest) = match default_folder(&parts[0].to_lowercase()) {
        Some(n) => (ns.call_obj("GetDefaultFolder", &[Var::Int(n)])?, &parts[1..]),
        None => (ns.call_obj("GetDefaultFolder", &[Var::Int(OL_FOLDER_INBOX)])?.get_obj("Parent")?, &parts[..]),
    };
    for name in rest {
        let children = folder.get_obj("Folders")?.items();
        match children.iter().find(|c| c.str_or_empty("Name").to_lowercase() == name.to_lowercase()) {
            Some(c) => folder = c.clone(),
            None => {
                let names: Vec<String> = children.iter().map(|c| c.str_or_empty("Name")).collect();
                let avail = if names.is_empty() { "(none)".into() } else { names.join(", ") };
                return Err(MailError::NotFound(format!(
                    "Folder '{name}' not found under '{}'. Available: {avail}", folder.str_or_empty("Name"))));
            }
        }
    }
    Ok(folder)
}

fn get_item(ns: &Disp, entry_id: &str) -> MailResult<Disp> {
    ns.call_obj("GetItemFromID", &[Var::Str(entry_id.into())])
        .map_err(|_| MailError::NotFound("Email not found (it may have been moved or deleted).".into()))
}

/// Folder items sorted by a date field, newest first. `Items` returns a new collection on every
/// access, so it is fetched once and kept.
fn sorted_items(folder: &Disp, field: &str, restrict: Option<&str>) -> MailResult<Disp> {
    let mut items = folder.get_obj("Items")?;
    if let Some(filter) = restrict {
        items = items.call_obj("Restrict", &[Var::Str(filter.into())])?;
    }
    items.call("Sort", &[Var::Str(format!("[{field}]")), Var::Bool(true)])?;
    Ok(items)
}

/// Walk a sorted Items collection with GetFirst/GetNext.
fn walk_items(items: &Disp, mut visit: impl FnMut(&Disp) -> bool) -> MailResult<()> {
    let mut item = items.call("GetFirst", &[])?.into_obj();
    while let Some(it) = item {
        if !visit(&it) {
            break;
        }
        item = items.call("GetNext", &[])?.into_obj();
    }
    Ok(())
}

fn cutoff(days: Option<i64>) -> Option<NaiveDateTime> {
    days.map(|d| Local::now().naive_local() - Duration::days(d))
}

fn draft_result(d: &Disp) -> SavedDraft {
    SavedDraft {
        entry_id: d.str_or_empty("EntryID"),
        subject: d.str_or_empty("Subject"),
        to: d.str_or_empty("To"),
        cc: d.str_or_empty("CC"),
    }
}

const OL_CLASS_APPOINTMENT: i64 = 26;
const OL_EMBEDDED_ITEM: i64 = 5;

fn is_meeting_like(item: &Disp) -> bool {
    let c = item.i64_or("Class", -1);
    c == OL_CLASS_APPOINTMENT || OL_MEETING_CLASSES.contains(&c)
}

/// The draft a forward starts from. Meeting invitations (and calendar items) become a normal email with
/// the invitation attached as an Outlook item, with a short details
/// block (title, time, place) as its body. Everything else uses Outlook's Forward().
/// Returns (draft, Some(details HTML) for a meeting); `style` decides the details' language and font.
fn build_forward(ns: &Disp, item: &Disp, style: &crate::text::BlockStyle, arabic: bool) -> MailResult<(Disp, Option<String>)> {
    if !is_meeting_like(item) {
        return Ok((item.call_obj("Forward", &[])?, None));
    }
    let mail = ns.get_obj("Application")?.call_obj("CreateItem", &[Var::Int(OL_MAIL_ITEM)])?;
    let subject = item.str_or_empty("Subject");
    mail.put("Subject", Var::Str(format!("FW: {subject}")))?;
    mail.get_obj("Attachments")?.call("Add", &[Var::Obj(item.clone()), Var::Int(OL_EMBEDDED_ITEM)])?;
    let appt = if item.i64_or("Class", -1) == OL_CLASS_APPOINTMENT { Some(item.clone()) } else {
        item.call("GetAssociatedAppointment", &[Var::Bool(false)]).ok().and_then(Var::into_obj)
    };
    let when = appt.as_ref().and_then(|a| {
        let start = a.date("Start")?;
        let end = a.date("End").map(|e| format!(" – {}", e.format("%H:%M"))).unwrap_or_default();
        Some(format!("{}{end}", start.format("%Y-%m-%d %H:%M")))
    });
    let place = appt.as_ref().map(|a| a.str_or_empty("Location")).filter(|l| !l.trim().is_empty());
    let details = crate::text::invitation_details(&subject, when.as_deref(), place.as_deref(), arabic || style.rtl, &style.font_family);
    Ok((mail, Some(details)))
}

/// How the user's text was written into an item.
pub enum Written {
    /// Mail items: a styled block at the top of HTMLBody.
    Html,
    /// Meeting invitations and other items without HTMLBody: inserted through Outlook's Word editor,
    /// which keeps the original content (e.g. Teams join links). The inspector must be closed after Save.
    Word(Disp),
    /// Last resort: plain text above the original body (formatting is lost).
    Plain,
}

/// Put the user's text at the top of a reply/forward/new item, keeping the original content.
fn write_message(item: &Disp, text: &str, style: &crate::text::BlockStyle) -> MailResult<Written> {
    if let Ok(Var::Str(html)) = item.get("HTMLBody") {
        item.put("HTMLBody", Var::Str(crate::text::insert_styled(Some(&html), text, style)))?;
        return Ok(Written::Html);
    }
    if let Ok(insp) = word_insert(item, text, style) {
        return Ok(Written::Word(insp));
    }
    let body = item.str_or_empty("Body");
    item.put("Body", Var::Str(format!("{text}\n\n{body}")))?;
    Ok(Written::Plain)
}

/// Insert formatted text at the start of the item's body via its (hidden) Word editor.
fn word_insert(item: &Disp, text: &str, s: &crate::text::BlockStyle) -> Result<Disp, ComError> {
    let insp = item.get_obj("GetInspector")?;
    let result = (|| {
        let doc = insp.get_obj("WordEditor")?;
        let rng = doc.call_obj("Range", &[Var::Int(0), Var::Int(0)])?;
        let word_text = format!("{}\r\r", text.replace("\r\n", "\n").replace('\n', "\r"));
        rng.call("InsertBefore", &[Var::Str(word_text)])?; // the range grows to cover the inserted text
        let font = rng.get_obj("Font")?;
        for (prop, val) in [("Name", &s.font_family), ("NameBi", &s.font_family)] {
            let _ = font.put(prop, Var::Str(val.clone()));
        }
        for prop in ["Size", "SizeBi"] {
            let _ = font.put(prop, Var::Float(s.font_size_pt as f64));
        }
        if s.rtl {
            let _ = rng.get_obj("ParagraphFormat").and_then(|pf| pf.put("ReadingOrder", Var::Int(0))); // wdReadingOrderRtl
        }
        Ok(())
    })();
    match result {
        Ok(()) => Ok(insp),
        Err(e) => {
            let _ = insp.call("Close", &[Var::Int(1)]); // olDiscard
            Err(e)
        }
    }
}

/// Save the item to Drafts and release the Word editor if one was used.
fn save_written(item: &Disp, w: Written) -> MailResult<()> {
    item.call("Save", &[])?; // lands in Drafts; deliberately no Send
    if let Written::Word(insp) = w {
        let _ = insp.call("Close", &[Var::Int(0)]); // olSave (already saved; just closes the hidden editor)
    }
    Ok(())
}

fn recipient_smtp(rcpt: &Disp) -> String {
    if let Ok(entry) = rcpt.get_obj("AddressEntry") {
        if let Ok(ex) = entry.call_obj("GetExchangeUser", &[]) {
            let smtp = ex.str_or_empty("PrimarySmtpAddress");
            if !smtp.is_empty() {
                return smtp;
            }
        }
        if let Ok(pa) = entry.get_obj("PropertyAccessor") {
            if let Ok(v) = pa.call("GetProperty", &[Var::Str(PR_SMTP_ADDRESS.into())]) {
                let s = v.into_string();
                if !s.is_empty() {
                    return s;
                }
            }
        }
    }
    rcpt.str_or_empty("Address")
}

/// Add and resolve recipients against the address book. Returns (resolved "Name <smtp>", unresolved).
/// Unresolved names stay in the draft as typed; Outlook's Check Names fixes them.
fn add_recipients(mail: &Disp, names: &[String], kind: i64) -> MailResult<(Vec<String>, Vec<String>)> {
    let recipients = mail.get_obj("Recipients")?;
    let (mut ok, mut bad) = (Vec::new(), Vec::new());
    for name in names {
        let r = recipients.call_obj("Add", &[Var::Str(name.clone())])?;
        r.put("Type", Var::Int(kind))?;
        if matches!(r.call("Resolve", &[]), Ok(Var::Bool(true))) {
            let smtp = recipient_smtp(&r);
            let display = r.str_or_empty("Name");
            ok.push(if smtp.is_empty() { display } else { format!("{display} <{smtp}>") });
        } else {
            bad.push(name.clone());
        }
    }
    Ok((ok, bad))
}

const PR_ATTACH_CONTENT_ID: &str = "http://schemas.microsoft.com/mapi/proptag/0x3712001F";

/// The body as HTML for the reading pane. Meeting invitations (and other items without `HTMLBody`)
/// keep their formatted body in `RTFBody`; usually that wraps the original HTML, which is recovered.
fn html_body(item: &Disp) -> String {
    let html = item.str_or_empty("HTMLBody");
    if !html.trim().is_empty() {
        return html;
    }
    match item.get("RTFBody") {
        Ok(Var::Bytes(rtf)) => crate::rtf::to_html(&rtf).unwrap_or_default(),
        _ => String::new(),
    }
}

/// Replace `cid:` references (inline pictures, signatures, posters) with `data:` URIs so the
/// reading pane can show them without any network access. Capped at 15 MB per email.
fn inline_cid_images(item: &Disp, html: String) -> String {
    use base64::Engine;
    if !html.to_ascii_lowercase().contains("cid:") {
        return html;
    }
    let Ok(atts) = item.get_obj("Attachments") else { return html };
    let dir = std::env::temp_dir().join("InboxChat").join("inline").join(uuid::Uuid::new_v4().to_string());
    if std::fs::create_dir_all(&dir).is_err() {
        return html;
    }
    let (mut out, mut budget) = (html, 15 * 1024 * 1024i64);
    for (i, a) in atts.items().iter().enumerate() {
        let cid = a.get_obj("PropertyAccessor")
            .and_then(|pa| pa.call("GetProperty", &[Var::Str(PR_ATTACH_CONTENT_ID.into())]))
            .map(Var::into_string).unwrap_or_default();
        let cid = cid.trim().trim_matches(['<', '>']);
        let size = a.i64_or("Size", 0);
        if cid.is_empty() || size > budget || !out.contains(&format!("cid:{cid}")) {
            continue;
        }
        let name = safe_file_name(&a.str_or_empty("FileName"));
        let path = dir.join(format!("{i}_{name}"));
        if a.call("SaveAsFile", &[Var::Str(path.display().to_string())]).is_err() {
            continue;
        }
        if let Ok(bytes) = std::fs::read(&path) {
            budget -= bytes.len() as i64;
            let mime = match name.rsplit('.').next().map(|e| e.to_ascii_lowercase()).as_deref() {
                Some("jpg") | Some("jpeg") => "image/jpeg",
                Some("gif") => "image/gif",
                Some("bmp") => "image/bmp",
                Some("webp") => "image/webp",
                _ => "image/png",
            };
            let uri = format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes));
            out = out.replace(&format!("cid:{cid}"), &uri);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    out
}

fn is_hidden_attachment(a: &Disp) -> bool {
    a.get_obj("PropertyAccessor")
        .and_then(|pa| pa.call("GetProperty", &[Var::Str(PR_ATTACHMENT_HIDDEN.into())]))
        .map(|v| matches!(v, Var::Bool(true)))
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------------- MailStore
impl MailStore for OutlookMail {
    fn list_folders(&self, max_depth: usize) -> MailResult<Vec<Folder>> {
        self.run(true, move |ns| {
            let root = ns.call_obj("GetDefaultFolder", &[Var::Int(OL_FOLDER_INBOX)])?.get_obj("Parent")?;
            let mut out = Vec::new();
            fn walk(folder: &Disp, prefix: &str, depth: usize, max: usize, out: &mut Vec<Folder>) -> MailResult<()> {
                for child in folder.get_obj("Folders")?.items() {
                    let name = child.str_or_empty("Name");
                    let path = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
                    out.push(Folder { path: path.clone(), unread: child.get("UnReadItemCount").ok().and_then(|v| v.as_i64()) });
                    if depth < max {
                        walk(&child, &path, depth + 1, max, out)?;
                    }
                }
                Ok(())
            }
            walk(&root, "", 1, max_depth, &mut out)?;
            Ok(out)
        })
    }

    fn folder_tree(&self, max_depth: usize) -> MailResult<Vec<FolderNode>> {
        self.run(true, move |ns| {
            let root = ns.call_obj("GetDefaultFolder", &[Var::Int(OL_FOLDER_INBOX)])?.get_obj("Parent")?;
            let specials: Vec<(String, &str)> = [(6, "inbox"), (16, "drafts"), (5, "sent"), (3, "deleted"), (23, "junk"), (4, "outbox")]
                .iter()
                .filter_map(|(n, k)| ns.call_obj("GetDefaultFolder", &[Var::Int(*n)]).ok().map(|f| (f.str_or_empty("EntryID"), *k)))
                .collect();
            fn walk(folder: &Disp, prefix: &str, depth: usize, max: usize, specials: &[(String, &str)]) -> MailResult<Vec<FolderNode>> {
                let mut out = Vec::new();
                for child in folder.get_obj("Folders")?.items() {
                    // Only mail folders (skip Calendar, Contacts, Tasks...): DefaultItemType 0 = mail.
                    if child.i64_or("DefaultItemType", 0) != OL_MAIL_ITEM {
                        continue;
                    }
                    let name = child.str_or_empty("Name");
                    let path = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
                    let eid = child.str_or_empty("EntryID");
                    let special = specials.iter().find(|(id, _)| *id == eid).map(|(_, k)| k.to_string());
                    let children = if depth < max { walk(&child, &path, depth + 1, max, specials)? } else { vec![] };
                    out.push(FolderNode {
                        unread: child.get("UnReadItemCount").ok().and_then(|v| v.as_i64()),
                        total: child.get_obj("Items").ok().and_then(|i| i.get("Count").ok()).and_then(|v| v.as_i64()),
                        path, name, special, children,
                    });
                }
                Ok(out)
            }
            walk(&root, "", 1, max_depth, &specials)
        })
    }

    fn list_messages(&self, folder: &str, days: Option<i64>, unread_only: bool, limit: usize) -> MailResult<Vec<Summary>> {
        let folder = folder.to_string();
        self.run(true, move |ns| {
            let items = sorted_items(&find_folder(ns, &folder)?, "ReceivedTime", unread_only.then_some("[UnRead] = True"))?;
            let cut = cutoff(days);
            let mut out = Vec::new();
            walk_items(&items, |it| {
                if out.len() >= limit {
                    return false;
                }
                if let (Some(c), Some(r)) = (cut, it.date("ReceivedTime")) {
                    if r < c {
                        return false; // sorted newest-first, so we're done
                    }
                }
                if is_supported(it) {
                    out.push(summary(it));
                }
                true
            })?;
            Ok(out)
        })
    }

    fn page(&self, folder: &str, offset: usize, limit: usize) -> MailResult<(Vec<Summary>, bool)> {
        let folder = folder.to_string();
        self.run(true, move |ns| {
            let f = find_folder(ns, &folder)?;
            let is_drafts = f.str_or_empty("EntryID")
                == ns.call_obj("GetDefaultFolder", &[Var::Int(OL_FOLDER_DRAFTS)])?.str_or_empty("EntryID");
            let items = sorted_items(&f, if is_drafts { "LastModificationTime" } else { "ReceivedTime" }, None)?;
            let (mut seen, mut out, mut more) = (0usize, Vec::new(), false);
            walk_items(&items, |it| {
                if !is_supported(it) {
                    return true;
                }
                if seen >= offset + limit {
                    more = true;
                    return false;
                }
                if seen >= offset {
                    let mut s = summary(it);
                    if is_drafts {
                        s.received = iso(it.date("LastModificationTime"));
                        s.from_name = format!("To: {}", it.str_or_empty("To"));
                    }
                    out.push(s);
                }
                seen += 1;
                true
            })?;
            Ok((out, more))
        })
    }

    fn search(&self, q: &SearchQuery, folders: &[String], days: Option<i64>, limit: usize) -> MailResult<Vec<Summary>> {
        let exact = dasl_filter(q, None)?;
        let words = fallback_words(&q.query);
        let loose = if words.is_empty() { None } else { Some(dasl_filter(q, Some(&words))?) };
        let folders: Vec<String> = if folders.is_empty() { vec!["inbox".into(), "sent".into()] } else { folders.to_vec() };
        let search_with = move |ns: &Disp, dasl: &str| -> MailResult<Vec<Summary>> {
            let cut = cutoff(days);
            let mut results = Vec::new();
            for fname in &folders {
                let items = sorted_items(&find_folder(ns, fname)?, "ReceivedTime", Some(dasl))?;
                let mut n = 0;
                walk_items(&items, |it| {
                    if n >= limit {
                        return false;
                    }
                    if let (Some(c), Some(r)) = (cut, it.date("ReceivedTime")) {
                        if r < c {
                            return false;
                        }
                    }
                    if is_supported(it) {
                        let mut s = summary(it);
                        s.folder = Some(fname.clone());
                        results.push(s);
                        n += 1;
                    }
                    true
                })?;
            }
            results.sort_by(|a, b| b.received.cmp(&a.received));
            results.truncate(limit);
            Ok(results)
        };
        self.run(true, move |ns| {
            let found = search_with(ns, &exact)?;
            match (&loose, found.is_empty()) {
                // Exact phrase found nothing: try all its (stable) words, e.g. موائمة vs مواءمة.
                (Some(l), true) => search_with(ns, l),
                _ => Ok(found),
            }
        })
    }

    fn later_in_conversation(&self, entry_id: &str) -> MailResult<Vec<ThreadEvent>> {
        let entry_id = entry_id.to_string();
        self.run(true, move |ns| {
            let item = get_item(ns, &entry_id)?;
            let base = item.date("SentOn").or_else(|| item.date("ReceivedTime"));
            let drafts_id = ns.call_obj("GetDefaultFolder", &[Var::Int(OL_FOLDER_DRAFTS)])?.str_or_empty("EntryID");
            // The whole conversation, across folders (Inbox subfolders included).
            let mut found: Vec<Disp> = Vec::new();
            if let Ok(Some(conv)) = item.call("GetConversation", &[]).map(Var::into_obj) {
                fn walk(conv: &Disp, node: Disp, out: &mut Vec<Disp>) {
                    out.push(node.clone());
                    if let Ok(children) = conv.call_obj("GetChildren", &[Var::Obj(node)]) {
                        for child in children.items() {
                            walk(conv, child, out);
                        }
                    }
                }
                if let Ok(roots) = conv.call_obj("GetRootItems", &[]) {
                    for root in roots.items() {
                        walk(&conv, root, &mut found);
                    }
                }
            }
            if found.len() <= 1 {
                // No conversation view: same topic in the Inbox.
                let topic = Some(item.str_or_empty("ConversationTopic")).filter(|t| !t.trim().is_empty())
                    .unwrap_or_else(|| topic_of(&item.str_or_empty("Subject")));
                let filter = format!("@SQL=\"urn:schemas:httpmail:thread-topic\" = '{}'", topic.replace('\'', "''"));
                let inbox = ns.call_obj("GetDefaultFolder", &[Var::Int(OL_FOLDER_INBOX)])?;
                let items = sorted_items(&inbox, "ReceivedTime", Some(&filter))?;
                walk_items(&items, |it| {
                    found.push(it.clone());
                    found.len() < 50
                })?;
            }
            let mut seen = std::collections::HashSet::new();
            let mut out: Vec<ThreadEvent> = found.iter()
                .filter(|it| it.i64_or("Class", -1) == OL_CLASS_MAIL)
                .filter(|it| it.get_obj("Parent").map(|p| p.str_or_empty("EntryID") != drafts_id).unwrap_or(true))
                .filter_map(|it| {
                    let id = it.str_or_empty("EntryID");
                    let when = it.date("ReceivedTime").or_else(|| it.date("SentOn"));
                    let later = matches!((base, when), (Some(b), Some(w)) if w > b);
                    (id != entry_id && later && seen.insert(id.clone())).then(|| ThreadEvent {
                        entry_id: id, from_name: it.str_or_empty("SenderName"), from: sender_smtp(it), when: iso(when),
                    })
                })
                .collect();
            out.sort_by(|a, b| a.when.cmp(&b.when));
            Ok(out)
        })
    }

    fn related(&self, entry_id: &str) -> MailResult<Related> {
        let entry_id = entry_id.to_string();
        self.run(true, move |ns| {
            let item = get_item(ns, &entry_id)?;
            // Outlook's own "You replied/forwarded on …" record.
            let (action, at) = last_action(&item);
            let mut out = Related { last_action: action, last_action_at: at, items: vec![] };
            // Replies, forwards and drafts in the same conversation.
            let topic = Some(item.str_or_empty("ConversationTopic")).filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| item.str_or_empty("Subject"));
            let since = item.date("ReceivedTime");
            let esc = topic.replace('\'', "''");
            let by_topic = format!("@SQL=\"urn:schemas:httpmail:thread-topic\" = '{esc}'");
            let by_subject = format!("@SQL=\"urn:schemas:httpmail:subject\" LIKE '%{esc}%'");
            for (folder_no, folder, date_field) in [(5, "sent", "SentOn"), (OL_FOLDER_DRAFTS, "drafts", "LastModificationTime")] {
                let f = ns.call_obj("GetDefaultFolder", &[Var::Int(folder_no)])?;
                let mut found = Vec::new();
                for filter in [&by_topic, &by_subject] {
                    let items = sorted_items(&f, date_field, Some(filter))?;
                    walk_items(&items, |it| {
                        if found.len() >= 20 {
                            return false;
                        }
                        let when = it.date(date_field);
                        if let (Some(s), Some(w)) = (since, when) {
                            if w < s {
                                return false; // older than the email itself: not a response to it
                            }
                        }
                        if it.i64_or("Class", -1) == OL_CLASS_MAIL {
                            let subject = it.str_or_empty("Subject");
                            found.push(RelatedItem {
                                entry_id: it.str_or_empty("EntryID"),
                                folder: folder.into(),
                                kind: kind_from_subject(&subject).into(),
                                subject,
                                to: it.str_or_empty("To"),
                                cc: it.str_or_empty("CC"),
                                when: iso(when),
                                preview: truncate_chars(&strip_quoted(&body_text(it)), PREVIEW_CHARS),
                            });
                        }
                        true
                    })?;
                    if !found.is_empty() {
                        break;
                    }
                }
                out.items.extend(found);
            }
            Ok(out)
        })
    }

    fn get_message(&self, entry_id: &str) -> MailResult<Message> {
        let entry_id = entry_id.to_string();
        self.run(true, move |ns| {
            let item = get_item(ns, &entry_id)?;
            let s = summary(&item);
            let body = body_text(&item);
            let truncated = if body.chars().count() > BODY_CHARS {
                format!("{}\n[...truncated]", truncate_chars(&body, BODY_CHARS))
            } else { body };
            let names = item.get_obj("Attachments").map(|a| a.items().iter().map(|x| x.str_or_empty("FileName")).collect())
                .unwrap_or_default();
            Ok(Message {
                entry_id: s.entry_id, kind: s.kind, subject: s.subject, from_name: s.from_name, from: s.from,
                received: s.received, unread: s.unread, importance: s.importance, attachments: s.attachments,
                to: item.str_or_empty("To"), cc: item.str_or_empty("CC"), body: truncated, attachment_names: names,
                conversation: item.str_or_empty("ConversationTopic"),
            })
        })
    }

    fn get_thread(&self, entry_id: &str, max_messages: usize, chars: usize) -> MailResult<Vec<ThreadMsg>> {
        let entry_id = entry_id.to_string();
        self.run(true, move |ns| {
            let item = get_item(ns, &entry_id)?;
            let mut collected: Vec<Disp> = Vec::new();
            if let Ok(Some(conv)) = item.call("GetConversation", &[]).map(Var::into_obj) {
                fn walk(conv: &Disp, node: Disp, out: &mut Vec<Disp>) {
                    out.push(node.clone());
                    if let Ok(children) = conv.call_obj("GetChildren", &[Var::Obj(node)]) {
                        for child in children.items() {
                            walk(conv, child, out);
                        }
                    }
                }
                if let Ok(roots) = conv.call_obj("GetRootItems", &[]) {
                    for root in roots.items() {
                        walk(&conv, root, &mut collected);
                    }
                }
            }
            if collected.is_empty() {
                collected.push(item);
            }
            let mut seen = std::collections::HashSet::new();
            let mut msgs: Vec<ThreadMsg> = collected.iter().filter(|it| is_supported(it)).filter_map(|it| {
                let id = it.str_or_empty("EntryID");
                if !seen.insert(id.clone()) {
                    return None;
                }
                Some(ThreadMsg {
                    entry_id: id,
                    from_name: it.str_or_empty("SenderName"),
                    from: sender_smtp(it),
                    to: it.str_or_empty("To"),
                    received: iso(it.date("ReceivedTime")),
                    subject: it.str_or_empty("Subject"),
                    body: truncate_chars(&strip_quoted(&body_text(it)), chars),
                })
            }).collect();
            msgs.sort_by(|a, b| a.received.cmp(&b.received));
            let skip = msgs.len().saturating_sub(max_messages);
            Ok(msgs.split_off(skip))
        })
    }

    fn list_drafts(&self, limit: usize) -> MailResult<Vec<DraftInfo>> {
        self.run(true, move |ns| {
            let drafts = ns.call_obj("GetDefaultFolder", &[Var::Int(OL_FOLDER_DRAFTS)])?;
            let items = sorted_items(&drafts, "LastModificationTime", None)?;
            let mut out = Vec::new();
            walk_items(&items, |it| {
                if out.len() >= limit {
                    return false;
                }
                if it.i64_or("Class", -1) == OL_CLASS_MAIL {
                    let subject = it.str_or_empty("Subject");
                    out.push(DraftInfo {
                        entry_id: it.str_or_empty("EntryID"),
                        subject: if subject.is_empty() { "(no subject)".into() } else { subject },
                        to: it.str_or_empty("To"),
                        modified: iso(it.date("LastModificationTime")),
                        preview: truncate_chars(&strip_quoted(&clean_text(&it.str_or_empty("Body"))), PREVIEW_CHARS),
                    });
                }
                true
            })?;
            Ok(out)
        })
    }

    fn read_full(&self, entry_id: &str) -> MailResult<FullMessage> {
        let entry_id = entry_id.to_string();
        self.run(true, move |ns| {
            let item = get_item(ns, &entry_id)?;
            let s = summary(&item);
            let drafts_id = ns.call_obj("GetDefaultFolder", &[Var::Int(OL_FOLDER_DRAFTS)])?.str_or_empty("EntryID");
            let is_draft = item.get_obj("Parent").map(|p| p.str_or_empty("EntryID") == drafts_id).unwrap_or(false);
            let (action, action_at) = last_action(&item);
            let attachments = item.get_obj("Attachments").map(|a| a.items().iter().enumerate()
                .filter(|(_, x)| !is_hidden_attachment(x)) // inline images in the body
                .map(|(i, x)| AttachmentInfo { index: i as i64 + 1, name: x.str_or_empty("FileName"), size: x.i64_or("Size", 0) })
                .collect()).unwrap_or_default();
            Ok(FullMessage {
                entry_id: s.entry_id, kind: s.kind, subject: s.subject, from_name: s.from_name, from: s.from,
                to: item.str_or_empty("To"), cc: item.str_or_empty("CC"),
                received: if is_draft { iso(item.date("LastModificationTime")) } else { s.received },
                importance: s.importance, unread: s.unread, is_draft,
                html: inline_cid_images(&item, html_body(&item)), text: body_text(&item), attachments,
                last_action: action, last_action_at: action_at,
            })
        })
    }

    fn save_attachment(&self, entry_id: &str, index: i64, dir: &Path) -> MailResult<PathBuf> {
        let (entry_id, dir) = (entry_id.to_string(), dir.to_path_buf());
        self.run(true, move |ns| {
            let item = get_item(ns, &entry_id)?;
            let att = item.get_obj("Attachments")?.call_obj("Item", &[Var::Int(index)])
                .map_err(|_| MailError::NotFound(format!("Attachment {index} not found.")))?;
            std::fs::create_dir_all(&dir).map_err(|e| MailError::Outlook(format!("Cannot create {}: {e}", dir.display())))?;
            let path = dir.join(safe_file_name(&att.str_or_empty("FileName")));
            att.call("SaveAsFile", &[Var::Str(path.display().to_string())])?;
            Ok(path)
        })
    }

    fn resolve_name(&self, name: &str) -> MailResult<Option<(String, String)>> {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Ok(None);
        }
        self.run(true, move |ns| {
            let r = ns.call_obj("CreateRecipient", &[Var::Str(name.clone())])?;
            if matches!(r.call("Resolve", &[]), Ok(Var::Bool(true))) {
                let smtp = recipient_smtp(&r);
                return Ok(Some((r.str_or_empty("Name"), smtp)).filter(|(_, e)| e.contains('@')));
            }
            Ok(None) // unknown or ambiguous: never guessed
        })
    }

    fn mark_read(&self, entry_id: &str) -> MailResult<bool> {
        let entry_id = entry_id.to_string();
        self.run(false, move |ns| {
            let item = get_item(ns, &entry_id)?;
            if !item.bool_or_false("UnRead") {
                return Ok(false);
            }
            item.put("UnRead", Var::Bool(false))?;
            let _ = item.call("Save", &[]); // persists the read state; content is unchanged
            Ok(true)
        })
    }

    fn display(&self, entry_id: &str) -> MailResult<()> {
        let entry_id = entry_id.to_string();
        self.run(false, move |ns| {
            get_item(ns, &entry_id)?.call("Display", &[])?;
            Ok(())
        })
    }

    // -- write operations (Drafts only; never Send) ----------------------------------------
    fn create_reply_draft(&self, entry_id: &str, text: &DraftText, reply_all: bool) -> MailResult<SavedDraft> {
        if text.body.trim().is_empty() {
            return Err(MailError::Invalid("Reply body is empty.".into()));
        }
        let (entry_id, text) = (entry_id.to_string(), text.clone());
        self.run(false, move |ns| {
            let item = get_item(ns, &entry_id)?;
            let reply = item.call_obj(if reply_all { "ReplyAll" } else { "Reply" }, &[])?;
            let rendered = text.render(&to_names(&reply));
            let w = write_message(&reply, &rendered, &text.block_style())?;
            save_written(&reply, w)?;
            Ok(draft_result(&reply))
        })
    }

    fn create_forward_draft(&self, entry_id: &str, to: &str, text: &DraftText, cc: &str) -> MailResult<ForwardDraft> {
        let (to_names_in, cc_names) = (split_recipients(to), split_recipients(cc));
        if to_names_in.is_empty() {
            return Err(MailError::Invalid("A forward needs at least one recipient in 'to'.".into()));
        }
        let (entry_id, text) = (entry_id.to_string(), text.clone());
        self.run(false, move |ns| {
            let item = get_item(ns, &entry_id)?;
            let (fwd, meeting_details) = build_forward(ns, &item, &text.block_style(), crate::text::has_arabic(&text.body))?;
            let (ok_to, bad_to) = add_recipients(&fwd, &to_names_in, OL_TO)?;
            let (ok_cc, bad_cc) = add_recipients(&fwd, &cc_names, OL_CC)?;
            let w = match &meeting_details {
                // Invitation as attachment: the user's message above the invitation details.
                Some(details) => {
                    let base = format!("<html><body>{details}</body></html>");
                    let html = if text.body.trim().is_empty() { base } else {
                        crate::text::insert_styled(Some(&base), &text.render(&to_names(&fwd)), &text.block_style())
                    };
                    fwd.put("HTMLBody", Var::Str(html))?;
                    Written::Html
                }
                None if text.body.trim().is_empty() => Written::Html,
                None => write_message(&fwd, &text.render(&to_names(&fwd)), &text.block_style())?,
            };
            save_written(&fwd, w)?;
            Ok(ForwardDraft {
                entry_id: fwd.str_or_empty("EntryID"),
                subject: fwd.str_or_empty("Subject"),
                recipients: ok_to.into_iter().chain(ok_cc.into_iter().map(|r| format!("CC: {r}"))).collect(),
                attachments: attachment_count(&fwd),
                unresolved: bad_to.into_iter().chain(bad_cc).collect(),
            })
        })
    }

    fn create_new_draft(&self, to: &str, subject: &str, text: &DraftText, cc: &str) -> MailResult<ForwardDraft> {
        let (to_in, cc_in) = (split_recipients(to), split_recipients(cc));
        if to_in.is_empty() {
            return Err(MailError::Invalid("A draft needs at least one recipient in 'to'.".into()));
        }
        let (subject, text) = (subject.to_string(), text.clone());
        self.run(false, move |ns| {
            let mail = ns.get_obj("Application")?.call_obj("CreateItem", &[Var::Int(OL_MAIL_ITEM)])?;
            let (ok_to, bad_to) = add_recipients(&mail, &to_in, OL_TO)?;
            let (ok_cc, bad_cc) = add_recipients(&mail, &cc_in, OL_CC)?;
            mail.put("Subject", Var::Str(subject.clone()))?;
            let rendered = text.render(&to_names(&mail));
            mail.put("HTMLBody", Var::Str(crate::text::insert_styled(Some("<html><body></body></html>"), &rendered, &text.block_style())))?;
            save_written(&mail, Written::Html)?;
            Ok(ForwardDraft {
                entry_id: mail.str_or_empty("EntryID"),
                subject: mail.str_or_empty("Subject"),
                recipients: ok_to.into_iter().chain(ok_cc.into_iter().map(|r| format!("CC: {r}"))).collect(),
                attachments: 0,
                unresolved: bad_to.into_iter().chain(bad_cc).collect(),
            })
        })
    }

    fn recent_senders(&self, offset: usize, limit: usize) -> MailResult<(Vec<Sender>, bool)> {
        self.run(true, move |ns| {
            let items = sorted_items(&ns.call_obj("GetDefaultFolder", &[Var::Int(OL_FOLDER_INBOX)])?, "ReceivedTime", None)?;
            let (mut seen, mut out, mut more) = (0usize, Vec::new(), false);
            walk_items(&items, |it| {
                if it.i64_or("Class", -1) != OL_CLASS_MAIL {
                    return true;
                }
                if seen >= offset + limit {
                    more = true;
                    return false;
                }
                if seen >= offset {
                    out.push(Sender {
                        item_id: it.str_or_empty("EntryID"),
                        name: it.str_or_empty("SenderName"),
                        email: sender_smtp(it),
                        when: iso(it.date("ReceivedTime")),
                    });
                }
                seen += 1;
                true
            })?;
            Ok((out, more))
        })
    }

    fn whoami(&self) -> MailResult<(String, String)> {
        self.run(true, |ns| {
            let me = ns.get_obj("CurrentUser")?;
            let name = me.str_or_empty("Name");
            Ok((name, recipient_smtp(&me)))
        })
    }

    fn sent_items(&self, offset: usize, limit: usize) -> MailResult<(Vec<SentItem>, bool)> {
        self.run(true, move |ns| {
            let items = sorted_items(&ns.call_obj("GetDefaultFolder", &[Var::Int(5)])?, "SentOn", None)?;
            let (mut seen, mut out, mut more) = (0usize, Vec::new(), false);
            walk_items(&items, |it| {
                if it.i64_or("Class", -1) != OL_CLASS_MAIL {
                    return true;
                }
                if seen >= offset + limit {
                    more = true;
                    return false;
                }
                if seen >= offset {
                    out.push(SentItem {
                        entry_id: it.str_or_empty("EntryID"),
                        sent_at: iso(it.date("SentOn")),
                        to: it.str_or_empty("To"),
                        subject: it.str_or_empty("Subject"),
                        body: truncate_chars(&body_text(it), 8000),
                        recipients: it.get_obj("Recipients").map(|r| r.items().iter()
                            .map(|x| (x.str_or_empty("Name"), recipient_smtp(x))).collect()).unwrap_or_default(),
                    });
                }
                seen += 1;
                true
            })?;
            Ok((out, more))
        })
    }
}
