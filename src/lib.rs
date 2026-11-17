//! Inbox Chat desktop: chat with your classic Outlook mailbox through a local Ollama model or a cloud
//! LLM (OpenAI ChatGPT, Anthropic Claude, Google Gemini, Moonshot Kimi).
//! It can never send email — replies and new messages are saved to Drafts only.

pub mod agent;
pub mod context;
pub mod followup;
pub mod learn;
pub mod llm;
pub mod profile;
pub mod mail;
pub mod rtf;
#[cfg(windows)]
pub mod outlook;
pub mod settings;
pub mod store;
pub mod text;
pub mod tools;
pub mod transfer;

/// Sample mailbox for tests and `--demo` (try the app without Outlook).
pub mod fake_mail;

use agent::{display_history, health_for, ChatAgent};
#[cfg(not(windows))]
use mail::MailError;
use mail::MailStore;
use serde_json::{json, Value};
use settings::Settings;
use std::sync::{Arc, Mutex};
use store::Store;
use tools::MailTools;

pub struct AppState {
    agent: ChatAgent,
    store: Arc<Store>,
    mail: Arc<dyn MailStore>,
    /// One chat turn at a time (single-user desktop app).
    chat_lock: Mutex<()>,
    storage_warning: Option<String>,
    learner: Learner,
    /// --demo: sample mailbox, in-memory storage.
    demo: bool,
    /// The SQLite file in use (None in demo mode or when it could not be opened); shown in Help.
    data_file: Option<String>,
}

const KV_WELCOME_DONE: &str = "welcome_done";

/// Background job that learns reply examples and correspondents from Outlook.
#[derive(Default)]
struct Learner {
    progress: std::sync::atomic::AtomicUsize,
    running: std::sync::atomic::AtomicBool,
    last: Mutex<Option<Value>>,
    trigger: Mutex<Option<std::sync::mpsc::Sender<()>>>,
}

fn run_learning(st: &AppState) -> Value {
    use std::sync::atomic::Ordering;
    let ctx = &st.agent.tools.ctx;
    let p = ctx.profile();
    st.learner.running.store(true, Ordering::Relaxed);
    st.learner.progress.store(0, Ordering::Relaxed);
    let started = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
    let result = (|| -> Result<Value, String> {
        let mut out = json!({"at": started});
        if p.learning.enabled {
            let r = learn::learn(st.mail.as_ref(), &st.store, &p.style, &p.learning, &st.learner.progress).map_err(|e| e.to_string())?;
            out["scanned"] = json!(r.scanned);
            out["added"] = json!(r.added);
        } else {
            // Still index who the user writes to (recipient lookup), without storing reply text.
            let mut off = 0;
            while let Ok((batch, more)) = st.mail.sent_items(off, 25) {
                for it in &batch {
                    for (n, e) in &it.recipients {
                        let _ = st.store.add_contact(&it.entry_id, n, e, it.sent_at.as_deref());
                    }
                }
                off += batch.len();
                if !more || batch.is_empty() || off >= 400 {
                    break;
                }
            }
        }
        out["senders_added"] = json!(learn::index_senders(st.mail.as_ref(), &st.store, p.learning.days_back, 1500).map_err(|e| e.to_string())?);
        Ok(out)
    })();
    st.learner.running.store(false, Ordering::Relaxed);
    let v = result.unwrap_or_else(|e| json!({"at": started, "error": e}));
    *st.learner.last.lock().unwrap_or_else(|e| e.into_inner()) = Some(v.clone());
    v
}

/// Learn shortly after start, then every 30 minutes or when asked.
fn start_learner(st: Shared) {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    *st.learner.trigger.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
    std::thread::Builder::new().name("learner".into()).spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(4));
        loop {
            run_learning(&st);
            match rx.recv_timeout(std::time::Duration::from_secs(30 * 60)) {
                Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => break,
            }
        }
    }).expect("spawn learner");
}

type Shared = Arc<AppState>;

/// Run blocking work (Outlook COM, HTTP to the LLM, SQLite) off the UI thread.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| format!("internal error: {e}"))
}

#[tauri::command]
async fn health(state: tauri::State<'_, Shared>) -> Result<Value, String> {
    let st = state.inner().clone();
    blocking(move || {
        let outlook = match st.mail.list_folders(1) {
            Ok(f) => json!({"ok": true, "folders": f.len()}),
            Err(e) => json!({"ok": false, "error": e.to_string()}),
        };
        json!({"outlook": outlook, "llm": st.agent.health(), "storage_warning": st.storage_warning})
    }).await
}

#[tauri::command]
fn get_settings(state: tauri::State<'_, Shared>) -> Value {
    state.agent.settings.public()
}

/// Version, demo mode, and whether this is a new installation (nothing set up or imported yet).
#[tauri::command]
fn app_info(state: tauri::State<'_, Shared>) -> Value {
    let st = state.inner();
    let first_run = !st.demo && st.agent.settings.never_saved() && st.agent.tools.ctx.never_saved()
        && st.store.kv_get(KV_WELCOME_DONE).ok().flatten().is_none();
    json!({"version": env!("CARGO_PKG_VERSION"), "demo": st.demo, "first_run": first_run, "data_file": st.data_file})
}

#[tauri::command]
fn welcome_done(state: tauri::State<'_, Shared>) -> Result<(), String> {
    state.store.kv_set(KV_WELCOME_DONE, "1").map_err(|e| e.to_string())
}

/// Save the LLM settings and profile to a JSON file the user picks. None if they cancel.
#[tauri::command]
async fn export_settings(app: tauri::AppHandle, window: tauri::Window, state: tauri::State<'_, Shared>,
                         include_keys: bool) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let st = state.inner().clone();
    blocking(move || {
        let name = format!("inbox-chat-settings-{}.json", chrono::Local::now().format("%Y-%m-%d"));
        let Some(path) = app.dialog().file().set_parent(&window).set_title("Export settings")
            .add_filter("Inbox Chat settings", &["json"]).set_file_name(&name).blocking_save_file() else { return Ok(None) };
        let path = path.into_path().map_err(|e| e.to_string())?;
        let data = transfer::export(&st.agent.settings, &st.agent.tools.ctx, include_keys);
        let text = serde_json::to_string_pretty(&data).map_err(|e| e.to_string())?;
        std::fs::write(&path, text).map_err(|e| format!("Could not write {}: {e}", path.display()))?;
        Ok(Some(path.display().to_string()))
    }).await?
}

/// Load a settings file the user picks. None if they cancel.
#[tauri::command]
async fn import_settings(app: tauri::AppHandle, window: tauri::Window, state: tauri::State<'_, Shared>) -> Result<Option<Value>, String> {
    use tauri_plugin_dialog::DialogExt;
    let st = state.inner().clone();
    blocking(move || {
        let Some(path) = app.dialog().file().set_parent(&window).set_title("Import settings")
            .add_filter("Inbox Chat settings", &["json"]).blocking_pick_file() else { return Ok(None) };
        let path = path.into_path().map_err(|e| e.to_string())?;
        let size = std::fs::metadata(&path).map_err(|e| format!("Could not read {}: {e}", path.display()))?.len();
        if size > transfer::MAX_FILE_BYTES {
            return Err("That file is too large to be an Inbox Chat settings file.".into());
        }
        let text = std::fs::read_to_string(&path).map_err(|e| format!("Could not read {}: {e}", path.display()))?;
        let data: Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))
            .map_err(|e| format!("The file is not valid JSON: {e}"))?;
        let mut report = transfer::import(&st.agent.settings, &st.agent.tools.ctx, &data)?;
        let _ = st.store.kv_set(KV_WELCOME_DONE, "1");
        report["file"] = json!(path.display().to_string());
        Ok(Some(report))
    }).await?
}

#[tauri::command]
async fn save_settings(state: tauri::State<'_, Shared>, patch: Value) -> Result<Value, String> {
    let st = state.inner().clone();
    blocking(move || st.agent.settings.update(&patch)).await?
}

/// Check a provider with unsaved form values and list its models.
#[tauri::command]
async fn test_settings(state: tauri::State<'_, Shared>, patch: Value) -> Result<Value, String> {
    let st = state.inner().clone();
    blocking(move || st.agent.settings.preview(&patch).map(|cfg| health_for(&cfg))).await?
}

#[tauri::command]
async fn chat(state: tauri::State<'_, Shared>, conversation_id: Option<String>, message: String,
              viewing: Option<String>) -> Result<Value, String> {
    let st = state.inner().clone();
    blocking(move || {
        let message = message.trim().to_string();
        if message.is_empty() {
            return Err("empty message".to_string());
        }
        // Only pass on an id the registry knows (it came from the UI's own lists).
        let viewing = viewing.filter(|v| st.store.entry_id(v).ok().flatten().is_some());
        let _turn = st.chat_lock.lock().unwrap_or_else(|e| e.into_inner());
        let db = |e: rusqlite::Error| format!("Storage error: {e}");
        let existing = match &conversation_id {
            Some(id) if st.store.conversation_exists(id).map_err(db)? => Some(id.clone()),
            _ => None,
        };
        let history = match &existing {
            Some(id) => st.store.load_history(id).map_err(db)?,
            None => vec![],
        };
        let id = match existing {
            Some(id) => id,
            None => st.store.create_conversation(&text::truncate_chars(&message, 60)).map_err(db)?,
        };
        match st.agent.run_turn(&history, &message, Some(&id), viewing.as_deref()) {
            Ok(r) => {
                st.store.append_messages(&id, &r.new_messages).map_err(db)?;
                Ok(json!({"conversation_id": id, "reply": r.reply, "steps": r.steps, "provider": r.provider, "model": r.model,
                          "followups": agent::followups_from(&r.new_messages)}))
            }
            Err(e) => {
                if history.is_empty() {
                    let _ = st.store.delete_conversation(&id); // don't keep an empty conversation
                }
                Err(e.0)
            }
        }
    }).await?
}

#[tauri::command]
async fn list_conversations(state: tauri::State<'_, Shared>) -> Result<Value, String> {
    let st = state.inner().clone();
    blocking(move || st.store.list_conversations(200).map(|c| json!(c)).map_err(|e| e.to_string())).await?
}

#[tauri::command]
async fn load_conversation(state: tauri::State<'_, Shared>, id: String) -> Result<Value, String> {
    let st = state.inner().clone();
    blocking(move || {
        let h = st.store.load_history(&id).map_err(|e| e.to_string())?;
        let mut messages = display_history(&h);
        // Items dismissed since this conversation took place are hidden.
        for m in messages.iter_mut() {
            for key in ["open", "closed"] {
                if let Some(list) = m.pointer_mut(&format!("/followups/{key}")).and_then(Value::as_array_mut) {
                    list.retain(|i| i["id"].as_str().and_then(|s| st.store.entry_id(s).ok().flatten())
                        .is_none_or(|eid| !st.store.followup_dismissed(&eid).unwrap_or(false)));
                }
            }
        }
        Ok(json!({"id": id, "messages": messages}))
    }).await?
}

#[tauri::command]
async fn delete_conversation(state: tauri::State<'_, Shared>, id: String) -> Result<(), String> {
    let st = state.inner().clone();
    blocking(move || st.store.delete_conversation(&id).map_err(|e| e.to_string())).await?
}

// ------------------------------------------------------------------------------ three-pane UI
fn resolve(st: &AppState, id: &str) -> Result<String, String> {
    st.agent.tools.long(&json!(id))
}

#[tauri::command]
async fn folders(state: tauri::State<'_, Shared>) -> Result<Value, String> {
    let st = state.inner().clone();
    blocking(move || st.mail.folder_tree(6).map(|t| json!(t)).map_err(|e| e.to_string())).await?
}

#[tauri::command]
async fn messages(state: tauri::State<'_, Shared>, folder: String, offset: Option<usize>) -> Result<Value, String> {
    let st = state.inner().clone();
    blocking(move || {
        let (mut rows, more) = st.mail.page(&folder, offset.unwrap_or(0), 50).map_err(|e| e.to_string())?;
        st.agent.tools.ctx.tag(&mut rows);
        Ok(json!({"folder": folder, "items": st.agent.tools.shorten(&rows)?, "has_more": more}))
    }).await?
}

#[tauri::command]
async fn open_message(state: tauri::State<'_, Shared>, id: String) -> Result<Value, String> {
    let st = state.inner().clone();
    blocking(move || {
        let eid = resolve(&st, &id)?;
        let full = st.mail.read_full(&eid).map_err(|e| e.to_string())?;
        let rel = st.agent.tools.ctx.classify(&full.to, &full.cc, &full.subject, &full.text);
        // Viewing marks the email read in Outlook (Profile → Learning & behaviour; on by default).
        let marked = full.unread && !full.is_draft && st.agent.tools.ctx.profile().mark_read
            && st.mail.mark_read(&eid).unwrap_or(false);
        let mut v = st.agent.tools.shorten(&[full])?.remove(0);
        v["tags"] = json!(rel.tags);
        v["matched"] = json!(rel.matched);
        if marked {
            v["unread"] = json!(false);
            v["marked_read"] = json!(true);
        }
        Ok(v)
    }).await?
}

// ------------------------------------------------------------------------------ profile, learning, insights
#[tauri::command]
async fn get_profile(state: tauri::State<'_, Shared>) -> Result<Value, String> {
    let st = state.inner().clone();
    blocking(move || st.agent.tools.ctx.public()).await
}

#[tauri::command]
async fn save_profile(state: tauri::State<'_, Shared>, profile: Value) -> Result<Value, String> {
    let st = state.inner().clone();
    blocking(move || {
        let p: profile::Profile = serde_json::from_value(profile).map_err(|e| format!("Invalid profile: {e}"))?;
        st.agent.tools.ctx.save(p)?;
        Ok(st.agent.tools.ctx.public())
    }).await?
}

#[tauri::command]
fn learning_status(state: tauri::State<'_, Shared>) -> Value {
    use std::sync::atomic::Ordering;
    let st = state.inner();
    json!({
        "running": st.learner.running.load(Ordering::Relaxed),
        "scanned": st.learner.progress.load(Ordering::Relaxed),
        "examples": st.store.example_count().unwrap_or(0),
        "contacts": st.store.contact_count().unwrap_or(0),
        "last": *st.learner.last.lock().unwrap_or_else(|e| e.into_inner()),
    })
}

#[tauri::command]
fn learn_now(state: tauri::State<'_, Shared>) -> Result<(), String> {
    match &*state.learner.trigger.lock().unwrap_or_else(|e| e.into_inner()) {
        Some(tx) => tx.send(()).map_err(|_| "The learner has stopped.".to_string()),
        None => Err("Learning is not running.".into()),
    }
}

#[tauri::command]
async fn forget_learning(state: tauri::State<'_, Shared>) -> Result<(), String> {
    let st = state.inner().clone();
    blocking(move || st.store.forget_learning().map_err(|e| e.to_string())).await?
}

#[tauri::command]
async fn insights(state: tauri::State<'_, Shared>, id: String, force: Option<bool>) -> Result<Value, String> {
    let st = state.inner().clone();
    blocking(move || {
        let eid = resolve(&st, &id)?;
        st.agent.insights(&eid, force.unwrap_or(false)).map(|i| json!(i))
    }).await?
}

/// Hide a follow-up item from all future follow-up lists (or bring it back).
#[tauri::command]
async fn dismiss_followup(state: tauri::State<'_, Shared>, id: String, dismissed: Option<bool>) -> Result<Value, String> {
    let st = state.inner().clone();
    blocking(move || {
        let eid = resolve(&st, &id)?;
        st.store.set_followup_dismissed(&eid, dismissed.unwrap_or(true)).map_err(|e| e.to_string())?;
        Ok(json!({"dismissed_count": st.store.dismissed_followup_count().unwrap_or(0)}))
    }).await?
}

#[tauri::command]
async fn restore_followups(state: tauri::State<'_, Shared>) -> Result<usize, String> {
    let st = state.inner().clone();
    blocking(move || st.store.restore_all_followups().map_err(|e| e.to_string())).await?
}

/// Save the (possibly edited) suggested reply as a draft in the user's style.
#[tauri::command]
async fn save_reply(state: tauri::State<'_, Shared>, id: String, body: String, greeting_name: Option<String>,
                    reply_all: Option<bool>) -> Result<Value, String> {
    let st = state.inner().clone();
    blocking(move || {
        if body.trim().is_empty() {
            return Err("The reply is empty.".to_string());
        }
        let eid = resolve(&st, &id)?;
        let ctx = &st.agent.tools.ctx;
        let text = ctx.draft(&body, greeting_name.filter(|g| !g.trim().is_empty()));
        let d = st.mail.create_reply_draft(&eid, &text, reply_all.unwrap_or(false)).map_err(|e| e.to_string())?;
        st.agent.tools.processed(&eid);
        let sid = st.store.short_id(&d.entry_id).map_err(|e| e.to_string())?;
        let _ = st.store.log_draft(None, "suggested", &sid, &d.subject, &d.to);
        Ok(json!({"id": sid, "subject": d.subject, "to": d.to}))
    }).await?
}

#[tauri::command]
async fn message_thread(state: tauri::State<'_, Shared>, id: String) -> Result<Value, String> {
    let st = state.inner().clone();
    blocking(move || {
        let msgs = st.mail.get_thread(&resolve(&st, &id)?, 30, 20_000).map_err(|e| e.to_string())?;
        Ok(json!(st.agent.tools.shorten(&msgs)?))
    }).await?
}

#[tauri::command]
async fn open_in_outlook(state: tauri::State<'_, Shared>, id: String) -> Result<(), String> {
    let st = state.inner().clone();
    blocking(move || st.mail.display(&resolve(&st, &id)?).map_err(|e| e.to_string())).await?
}

fn attachment_dir(short_id: &str) -> std::path::PathBuf {
    std::env::temp_dir().join("InboxChat").join("attachments").join(mail::safe_file_name(short_id))
}

/// File types Outlook itself blocks (Level 1): never launched from this app.
const BLOCKED_EXT: &[&str] = &[
    "ade", "adp", "app", "application", "appref-ms", "asp", "aspx", "asx", "bas", "bat", "bgi", "cab", "cer", "chm",
    "cmd", "cnt", "com", "cpl", "crt", "csh", "der", "diagcab", "exe", "fxp", "gadget", "grp", "hlp", "hpj", "hta",
    "htc", "inf", "ins", "isp", "its", "jar", "jnlp", "js", "jse", "ksh", "lnk", "mad", "maf", "mag", "mam", "maq",
    "mar", "mas", "mat", "mau", "mav", "maw", "mcf", "mda", "mdb", "mde", "mdt", "mdw", "mdz", "msc", "msh", "msh1",
    "msh2", "mshxml", "msh1xml", "msh2xml", "msi", "msix", "msixbundle", "msp", "mst", "msu", "ops", "osd", "pcd",
    "pif", "pl", "plg", "prf", "prg", "printerexport", "ps1", "ps1xml", "ps2", "ps2xml", "psc1", "psc2", "psd1",
    "psdm1", "pst", "py", "pyc", "pyo", "pyw", "pyz", "pyzw", "reg", "scf", "scr", "sct", "settingcontent-ms", "shb",
    "shs", "theme", "tmp", "url", "vb", "vbe", "vbp", "vbs", "vhd", "vhdx", "vsmacros", "vsw", "webpnp", "website",
    "ws", "wsb", "wsc", "wsf", "wsh", "xbap", "xll", "xnk",
];

fn extension(path: &std::path::Path) -> String {
    path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// Mark the saved file as downloaded from the internet, as Outlook does, so Office opens it in
/// Protected View and Windows SmartScreen applies.
fn mark_of_the_web(path: &std::path::Path) {
    let ads = format!("{}:Zone.Identifier", path.display());
    let _ = std::fs::write(ads, "[ZoneTransfer]\r\nZoneId=3\r\n");
}

fn save_attachment(st: &AppState, id: &str, index: i64) -> Result<std::path::PathBuf, String> {
    let path = st.mail.save_attachment(&resolve(st, id)?, index, &attachment_dir(id)).map_err(|e| e.to_string())?;
    mark_of_the_web(&path);
    Ok(path)
}

#[tauri::command]
async fn attachment_open(state: tauri::State<'_, Shared>, id: String, index: i64) -> Result<(), String> {
    let st = state.inner().clone();
    blocking(move || {
        let path = save_attachment(&st, &id, index)?;
        let ext = extension(&path);
        if BLOCKED_EXT.contains(&ext.as_str()) {
            let _ = std::fs::remove_file(&path);
            return Err(format!(".{ext} files can run code, so this app won't open them. Open the email in Outlook if you trust it."));
        }
        shell_open(&path.display().to_string())
    }).await?
}

#[tauri::command]
async fn attachment_preview(state: tauri::State<'_, Shared>, id: String, index: i64) -> Result<Value, String> {
    use base64::Engine;
    let st = state.inner().clone();
    blocking(move || {
        let path = save_attachment(&st, &id, index)?;
        let ext = extension(&path);
        let mime = match ext.as_str() {
            "png" => "image/png", "jpg" | "jpeg" => "image/jpeg", "gif" => "image/gif", "bmp" => "image/bmp",
            "webp" => "image/webp", "pdf" => "application/pdf",
            "txt" | "csv" | "log" | "md" | "json" | "xml" | "ics" | "vcf" | "tsv" => "text/plain",
            _ => return Ok(json!({"previewable": false, "name": path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()})),
        };
        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        if bytes.len() > 25 * 1024 * 1024 {
            return Ok(json!({"previewable": false, "name": path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default(),
                             "reason": "Too large to preview."}));
        }
        Ok(json!({"previewable": true, "mime": mime,
                  "name": path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default(),
                  "data": base64::engine::general_purpose::STANDARD.encode(bytes)}))
    }).await?
}

/// Open a web link from an email in the default browser (http/https only).
#[tauri::command]
async fn open_url(url: String) -> Result<(), String> {
    let lower = url.trim().to_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("http://")) {
        return Err("Only web links (http/https) can be opened.".into());
    }
    blocking(move || shell_open(url.trim())).await?
}

#[cfg(windows)]
fn shell_open(target: &str) -> Result<(), String> {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE};
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        let r = ShellExecuteW(None, &HSTRING::from("open"), &HSTRING::from(target), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
        if r.0 as isize <= 32 {
            return Err(format!("Windows could not open it (no app is registered for this file type?) [code {}]", r.0 as isize));
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn shell_open(_target: &str) -> Result<(), String> {
    Err("Opening files is only supported on Windows.".into())
}

#[cfg(windows)]
fn mail_store() -> Arc<dyn MailStore> {
    Arc::new(outlook::OutlookMail::start())
}

#[cfg(not(windows))]
fn mail_store() -> Arc<dyn MailStore> {
    struct NoOutlook;
    impl MailStore for NoOutlook {
        fn list_folders(&self, _: usize) -> mail::MailResult<Vec<mail::Folder>> { Err(no()) }
        fn list_messages(&self, _: &str, _: Option<i64>, _: bool, _: usize) -> mail::MailResult<Vec<mail::Summary>> { Err(no()) }
        fn search(&self, _: &str, _: &str, _: &[String], _: Option<i64>, _: usize) -> mail::MailResult<Vec<mail::Summary>> { Err(no()) }
        fn get_message(&self, _: &str) -> mail::MailResult<mail::Message> { Err(no()) }
        fn get_thread(&self, _: &str) -> mail::MailResult<Vec<mail::ThreadMsg>> { Err(no()) }
        fn list_drafts(&self, _: usize) -> mail::MailResult<Vec<mail::DraftInfo>> { Err(no()) }
        fn create_reply_draft(&self, _: &str, _: &str, _: bool) -> mail::MailResult<mail::SavedDraft> { Err(no()) }
        fn create_new_draft(&self, _: &str, _: &str, _: &str, _: &str) -> mail::MailResult<mail::SavedDraft> { Err(no()) }
    }
    fn no() -> MailError {
        MailError::Outlook("Outlook is only available on Windows.".into())
    }
    Arc::new(NoOutlook)
}

pub fn run() {
    // --demo (or INBOX_CHAT_DEMO=1): sample mailbox, nothing saved, real Outlook untouched.
    let demo = std::env::args().any(|a| a == "--demo") || std::env::var("INBOX_CHAT_DEMO").as_deref() == Ok("1");
    let path = Store::default_path();
    let (store, storage_warning, data_file) = if demo {
        (Store::memory().expect("in-memory SQLite"),
         Some("Demo mode: a sample mailbox, not your Outlook. Nothing is saved.".to_string()), None)
    } else {
        match Store::open(&path) {
            Ok(s) => (s, None, Some(path.display().to_string())),
            Err(e) => (
                Store::memory().expect("in-memory SQLite"),
                Some(format!("Could not open {} ({e}). Nothing will be saved this session.", path.display())),
                None,
            ),
        }
    };
    let store = Arc::new(store);
    let mail: Arc<dyn MailStore> = if demo { Arc::new(fake_mail::FakeMail::demo()) } else { mail_store() };
    let settings = Arc::new(Settings::load(store.clone()));
    let agent = ChatAgent::new(MailTools::new(mail.clone(), store.clone()), settings);
    if demo {
        // The sample user's profile, so the demo shows greetings, sign-off and relevance tags.
        let _ = agent.tools.ctx.save(fake_mail::sample_profile());
    }
    let state: Shared = Arc::new(AppState { agent, store, mail, chat_lock: Mutex::new(()), storage_warning,
                                            learner: Learner::default(), demo, data_file });
    start_learner(state.clone());

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            health, get_settings, save_settings, test_settings, chat, app_info, welcome_done, export_settings, import_settings,
            list_conversations, load_conversation, delete_conversation,
            folders, messages, open_message, message_thread, open_in_outlook,
            attachment_open, attachment_preview, open_url,
            get_profile, save_profile, learning_status, learn_now, forget_learning, insights, save_reply,
            dismiss_followup, restore_followups
        ])
        .run(tauri::generate_context!())
        .expect("error while running Inbox Chat");
}
