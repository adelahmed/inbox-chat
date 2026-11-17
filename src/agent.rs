//! Tool-calling chat loop. Each turn runs on a copy of the history and returns only the new messages,
//! so a failed turn leaves the stored conversation untouched (no dangling tool calls).

use crate::llm::{Backend, LlmError};
use crate::settings::Settings;
use crate::tools::{describe_call, tool_specs, MailTools};
use regex::Regex;
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::{Arc, LazyLock};

const SYSTEM_PROMPT: &str = "You are a private email assistant working inside the user's own Outlook mailbox{user}.
Today is {today}.

How to work:
- Use the tools to find facts. Never invent emails, senders, dates or content.
- Refer to emails with their short id in brackets, e.g. [m3], so the user can say \"reply to m3\".
- Summaries: group by topic or urgency, lead with what needs the user's action or a decision, note deadlines.
- Before drafting a reply, call read_thread on that email. Match the formality of the thread.
- Put email addresses in to/cc. If you only have a name, call find_recipient first; if several people match,
  ask the user which one — never guess.
- \"What do I need to follow up?\" / «ما الذي يحتاج متابعة؟»: call find_followups (mailbox-wide, no id; last 10 days).
  It returns only requests still waiting for an answer. Answer in a few lines: who, what was asked, how long it has
  waited, and whether the user already chased it. No status labels or tags. The app shows the list with
  Open/Dismiss buttons under your answer, so don't repeat every detail.
- \"Did I reply / forward / do anything with this?\": call find_related on that email. Don't search Sent Items by
  sender — there the sender is always the user; use the recipient filter instead.
- To forward, find the email, then call create_forward_draft with the user's message as body. If the result
  lists \"unresolved\" names, tell the user and show the candidates.
- You can only SAVE drafts. You cannot send email. After saving, say it is in the Drafts folder for review.
- If the user asks for a reply but has not said what it should say and the thread doesn't make it obvious,
  ask a short question instead of guessing.
- Answer in the language the user writes in. Be concise.";

const MAX_STEPS_REPLY: &str =
    "I stopped after too many tool calls without reaching an answer. Try a more specific question.";

static THINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<think>.*?</think>").unwrap());

/// The ✨ panel's task; `lang` is the profile's language, or empty for the email's own language.
fn insights_task(lang: &str) -> String {
    let lang = if lang.trim().is_empty() { "the language of the email".to_string() } else { lang.trim().to_string() };
    format!("Task: analyse the email below for the user. Return ONLY a JSON object (no prose, no code fence) with:
- \"summary\": 2–4 sentences in {lang} summarising the whole thread: who wants what, and where it stands;
- \"actions\": an array of short items in {lang} — what the user personally needs to do, with any deadline; [] if nothing;
- \"needs_reply\": true if the user should reply;
- \"greeting_name\": how the user would address the sender, with a title if fitting (e.g. \"Dr. Sara\", \"الأستاذ خالد\");
- \"reply\": if needs_reply, the body of the user's reply in {lang}, in the user's style — the message only, no greeting, no sign-off; otherwise \"\".
Never invent facts, dates or commitments: where the user must decide something, keep the reply short and put a clear placeholder such as [date].")
}

/// The email open in the reading pane: the default subject of every chat instruction.
pub struct OpenEmail {
    pub id: String,
    pub subject: String,
    pub from: String,
    header: String,
    pub text: String,
    kind: String,
}

impl OpenEmail {
    fn prompt_block(&self) -> String {
        let id = &self.id;
        format!(
            "\n\nOPEN EMAIL — the default subject of every instruction.\n\
             The user has email [{id}] ({kind}) open in the reading pane. Unless the message clearly points elsewhere \
             (another email id, sender, subject, folder, \"my inbox\"/\"emails from …\", or a mailbox-wide question such as \
             follow-ups), apply the instruction to [{id}]: \
             \"reply\", \"forward to X\", \"summarize\", \"what do they want\", \"translate\", \"who is in CC\" all mean this \
             email. Do not search for it — use id \"{id}\" directly. Use read_thread on {id} for earlier messages.\n\
             [{id}] Subject: {subject}\n{header}\nBody (quoted history removed):\n{text}",
            kind = self.kind, subject = self.subject, header = self.header, text = self.text)
    }
}

/// The reading-pane panel for one email.
#[derive(Debug, Clone, Default, Serialize, serde::Deserialize, PartialEq)]
#[serde(default)]
pub struct Insights {
    pub summary: String,
    pub actions: Vec<String>,
    pub needs_reply: bool,
    pub greeting_name: String,
    pub reply: String,
    pub examples_used: usize,
    pub provider: String,
    pub model: String,
    #[serde(skip_deserializing)]
    pub cached: bool,
}

/// Lenient JSON extraction: models sometimes wrap JSON in prose or code fences.
pub fn parse_insights(content: &str) -> Insights {
    let text = content.trim();
    let json_part = match (text.find('{'), text.rfind('}')) {
        (Some(a), Some(b)) if b > a => &text[a..=b],
        _ => "",
    };
    let Ok(v) = serde_json::from_str::<Value>(json_part) else {
        return Insights { summary: text.to_string(), ..Default::default() };
    };
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
    let actions = match v.get("actions") {
        Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str().map(|s| s.trim().to_string())).filter(|s| !s.is_empty()).collect(),
        Some(Value::String(s)) => s.lines().map(|l| l.trim().trim_start_matches(['-', '•', '*']).trim().to_string()).filter(|l| !l.is_empty()).collect(),
        _ => vec![],
    };
    let needs_reply = match v.get("needs_reply") {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(x)) => x.eq_ignore_ascii_case("true"),
        _ => !s("reply").is_empty(),
    };
    Insights { summary: s("summary"), actions, needs_reply, greeting_name: s("greeting_name"), reply: s("reply"), ..Default::default() }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Step {
    pub tool: String,
    pub label: String,
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TurnResult {
    pub reply: String,
    pub steps: Vec<Step>,
    pub provider: String,
    pub model: String,
    #[serde(skip)]
    pub new_messages: Vec<Value>,
}

pub struct ChatAgent {
    pub tools: MailTools,
    pub settings: Arc<Settings>,
    pub max_steps: usize,
}

fn result_ok(result: &str) -> bool {
    !result.chars().take(12).collect::<String>().contains("\"error\"")
}

/// Keep recent history; shrink old tool outputs so the context doesn't overflow.
pub fn compact(history: &[Value], keep: usize) -> Vec<Value> {
    let mut hist: &[Value] = &history[history.len().saturating_sub(keep)..];
    while hist.first().is_some_and(|m| m["role"] != "user") {
        hist = &hist[1..];
    }
    let last_user = hist.iter().rposition(|m| m["role"] == "user").unwrap_or(0);
    hist.iter().enumerate().map(|(i, m)| {
        let content = m["content"].as_str().unwrap_or("");
        if m["role"] == "tool" && i < last_user && content.chars().count() > 1500 {
            let mut m = m.clone();
            m["content"] = json!(format!("{} ...\"[older result truncated]\"", content.chars().take(1500).collect::<String>()));
            m
        } else {
            m.clone()
        }
    }).collect()
}

impl ChatAgent {
    pub fn new(tools: MailTools, settings: Arc<Settings>) -> Self {
        ChatAgent { tools, settings, max_steps: 8 }
    }

    pub fn health(&self) -> Value {
        health_for(&self.settings.get())
    }

    fn system(&self, open: Option<&OpenEmail>) -> String {
        let today = chrono::Local::now().format("%A %d %B %Y, %H:%M").to_string();
        let name = self.settings.get().user_name;
        let user = if name.is_empty() { String::new() } else { format!(" (the user is {name})") };
        let mut s = SYSTEM_PROMPT.replace("{user}", &user).replace("{today}", &today);
        let about = open.map(|o| (o.from.as_str(), o.subject.as_str(), o.text.as_str()));
        s.push_str(&self.tools.ctx.prompt_section(about));
        if let Some(o) = open {
            s.push_str(&o.prompt_block());
        }
        s
    }

    /// The email open in the reading pane, read once per turn.
    fn open_email(&self, viewing: Option<&str>) -> Option<OpenEmail> {
        let id = viewing?.to_string();
        let eid = self.tools.long(&json!(id)).ok()?;
        let m = self.tools.mail.read_full(&eid).ok()?;
        let tags = self.tools.ctx.classify(&m.to, &m.cc, &m.subject, &m.text).tags;
        let attachments = if m.attachments.is_empty() { "-".to_string() } else {
            m.attachments.iter().map(|a| a.name.clone()).collect::<Vec<_>>().join(", ")
        };
        let status = match &m.last_action {
            Some(a) => format!("\nStatus in Outlook: the user {a} this email{}.",
                               m.last_action_at.as_ref().map(|t| format!(" on {t}")).unwrap_or_default()),
            None => "\nStatus in Outlook: no reply or forward recorded (use find_related to check Sent Items/Drafts).".into(),
        };
        let header = format!("From: {} <{}>\nTo: {}\nCc: {}\nDate: {}\nAttachments: {attachments}{}{status}",
                             m.from_name, m.from, m.to, if m.cc.is_empty() { "-" } else { &m.cc },
                             m.received.clone().unwrap_or_default(),
                             if tags.is_empty() { String::new() } else { format!("\nTags: {}", tags.join(", ")) });
        Some(OpenEmail {
            id,
            header,
            text: crate::text::truncate_chars(&crate::text::strip_quoted(&m.text), 3000),
            kind: if m.is_draft { "draft".into() } else { m.kind },
            subject: m.subject,
            from: m.from_name,
        })
    }

    /// Reading-pane analysis of one email: thread summary, what the user must do, and a suggested
    /// reply in their style. Cached per email; `force` regenerates.
    pub fn insights(&self, entry_id: &str, force: bool) -> Result<Insights, String> {
        let store = &self.tools.store;
        if !force {
            if let Some(cached) = store.insight(entry_id).ok().flatten().and_then(|s| serde_json::from_str::<Insights>(&s).ok()) {
                return Ok(Insights { cached: true, ..cached });
            }
        }
        let ctx = &self.tools.ctx;
        let m = self.tools.mail.read_full(entry_id).map_err(|e| e.to_string())?;
        let thread = self.tools.mail.get_thread(entry_id, 8, 1500).unwrap_or_default();
        let rel = ctx.classify(&m.to, &m.cc, &m.subject, &m.text);
        let from_me = ctx.me().iter().any(|me| crate::profile::name_matches(&m.from_name, me) || me.eq_ignore_ascii_case(&m.from));
        let backend = Backend::new(&self.settings.get()).map_err(|e| e.0)?;
        let today = chrono::Local::now().format("%A %d %B %Y").to_string();
        let system = format!("You are the user's private email assistant. Today is {today}.{}\n\n{}",
                             ctx.prompt_section(Some((&m.from_name, &m.subject, &m.text))), insights_task(&ctx.profile().language));
        let earlier: Vec<String> = thread.iter().filter(|t| t.entry_id != m.entry_id)
            .map(|t| format!("— {} ({}):\n{}", t.from_name, t.received.clone().unwrap_or_default(), t.body)).collect();
        let done = m.last_action.as_ref().map(|a| format!(" — the user already {a} it{}, so needs_reply is false unless something is still open",
            m.last_action_at.as_ref().map(|t| format!(" on {t}")).unwrap_or_default())).unwrap_or_default();
        let user = format!(
            "Email to analyse{}{}{done}:\nFrom: {} <{}>\nTo: {}\nCc: {}\nDate: {}\nSubject: {}\n\n{}\n\nEarlier messages in the thread, oldest first:\n{}",
            if rel.tags.is_empty() { String::new() } else { format!(" (tags: {})", rel.tags.join(", ")) },
            if from_me { " — written BY the user, so needs_reply is false" } else { "" },
            m.from_name, m.from, m.to, m.cc, m.received.clone().unwrap_or_default(), m.subject,
            crate::text::truncate_chars(&m.text, 4000), if earlier.is_empty() { "(none)".into() } else { earlier.join("\n\n") });
        let c = backend.complete_with(&system, &[json!({"role": "user", "content": user})], &[], true).map_err(|e| e.0)?;
        let mut out = parse_insights(&THINK.replace_all(&c.content, ""));
        let style = ctx.profile().style;
        if style.enabled {
            out.reply = crate::profile::strip_own_greeting_signoff(&style, &out.reply);
        }
        if from_me || m.is_draft {
            out.needs_reply = false;
            out.reply.clear();
        }
        out.examples_used = ctx.example_count().min(4) as usize;
        out.provider = backend.provider.clone();
        out.model = backend.model.clone();
        if let Ok(s) = serde_json::to_string(&out) {
            let _ = store.set_insight(entry_id, &s);
        }
        Ok(out)
    }

    /// Run one user turn on top of `history`. Returns the reply and the messages to append.
    /// `viewing` is the short id of the email open in the reading pane, if any.
    pub fn run_turn(&self, history: &[Value], user_message: &str, conversation_id: Option<&str>,
                    viewing: Option<&str>) -> Result<TurnResult, LlmError> {
        let backend = Backend::new(&self.settings.get())?;
        let specs = tool_specs();
        let open = self.open_email(viewing);
        let mut hist = history.to_vec();
        let start = hist.len();
        let mut user = json!({"role": "user", "content": user_message});
        if let Some(o) = &open {
            // Remember which email this message was about, so switching emails mid-conversation stays clear.
            user["viewing"] = json!(o.id);
            user["viewing_subject"] = json!(o.subject);
        }
        hist.push(user);
        let mut steps = Vec::new();
        let finish = |hist: Vec<Value>, reply: String, steps: Vec<Step>| TurnResult {
            reply, steps, provider: backend.provider.clone(), model: backend.model.clone(),
            new_messages: hist[start..].to_vec(),
        };
        // Fixed for the whole turn: within a turn every request only appends to the previous one. Claude's
        // thinking blocks are bound to everything sent before them (a changed system prompt, e.g. a new
        // minute in the date, or a re-trimmed history would invalidate them); it also keeps prompt caches warm.
        let system = self.system(open.as_ref());
        let prefix = compact(&hist, 40);

        for _ in 0..self.max_steps {
            let mut wire = prefix.clone();
            wire.extend_from_slice(&hist[start + 1..]);
            let msg = backend.complete(&system, &wire, &specs)?;
            let content = THINK.replace_all(&msg.content, "").trim().to_string();
            let calls: Vec<Value> = msg.tool_calls.iter().enumerate().map(|(n, c)| {
                let args = match &c.arguments {
                    Value::String(s) if s.trim().is_empty() => json!({}),
                    Value::String(s) => serde_json::from_str::<Value>(s).ok().filter(Value::is_object).unwrap_or(json!({})),
                    v if v.is_object() => v.clone(),
                    _ => json!({}),
                };
                let mut call = json!({
                    "id": c.id.clone().unwrap_or_else(|| format!("call_{}_{n}", hist.len())),
                    "name": c.name, "arguments": args,
                });
                if let Some(extra) = &c.extra {
                    call["extra"] = Value::Object(extra.clone());
                }
                call
            }).collect();

            let mut entry = json!({"role": "assistant", "content": content});
            if !calls.is_empty() {
                entry["tool_calls"] = json!(calls);
            }
            if let Some(r) = &msg.reasoning {
                entry["reasoning"] = json!(r);
            }
            if let Some(raw) = &msg.raw {
                entry["raw"] = json!({"provider": backend.provider, "content": raw});
            }
            hist.push(entry);
            if calls.is_empty() {
                let reply = if content.is_empty() { "(no answer)".to_string() } else { content };
                return Ok(finish(hist, reply, steps));
            }
            for c in &calls {
                let name = c["name"].as_str().unwrap_or("");
                let result = self.tools.call_with(name, &c["arguments"], conversation_id, open.as_ref().map(|o| o.id.as_str()));
                steps.push(Step { tool: name.into(), label: describe_call(name, &c["arguments"]), ok: result_ok(&result) });
                hist.push(json!({"role": "tool", "content": result, "tool_call_id": c["id"], "tool_name": name}));
            }
        }
        hist.push(json!({"role": "assistant", "content": MAX_STEPS_REPLY}));
        Ok(finish(hist, MAX_STEPS_REPLY.into(), steps))
    }
}

/// The follow-up list produced in these messages (last find_followups result), for the UI's list.
pub fn followups_from(messages: &[Value]) -> Option<Value> {
    messages.iter().rev()
        .filter(|m| m["role"] == "tool" && m["tool_name"] == "find_followups")
        .find_map(|m| serde_json::from_str::<Value>(m["content"].as_str()?).ok().filter(|v| v.get("open").is_some()))
}

pub fn health_for(cfg: &crate::settings::Config) -> Value {
    match Backend::new(cfg) {
        Ok(b) => b.health(),
        Err(e) => json!({"ok": false, "error": e.0, "provider": cfg.provider, "model": cfg.active().model,
                         "label": crate::settings::provider_info(&cfg.provider).map(|p| p.label).unwrap_or(""),
                         "local": cfg.provider == "ollama"}),
    }
}

/// Turn stored history into chat bubbles for the UI: user messages and final answers with the
/// tool steps that led to them.
pub fn display_history(history: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    let mut steps: Vec<Value> = Vec::new();
    let mut pending: Vec<(String, String, String)> = Vec::new(); // (id, tool, label)
    let mut followups: Option<Value> = None;
    for m in history {
        match m["role"].as_str() {
            Some("user") => {
                steps.clear();
                pending.clear();
                out.push(json!({"role": "user", "text": m["content"], "viewing": m["viewing"]}));
            }
            Some("assistant") => {
                if let Some(calls) = m["tool_calls"].as_array().filter(|c| !c.is_empty()) {
                    for c in calls {
                        let name = c["name"].as_str().unwrap_or("").to_string();
                        pending.push((c["id"].as_str().unwrap_or("").into(), name.clone(), describe_call(&name, &c["arguments"])));
                    }
                } else {
                    let mut bubble = json!({"role": "assistant", "text": m["content"], "steps": steps.clone()});
                    if let Some(f) = followups.take() {
                        bubble["followups"] = f;
                    }
                    out.push(bubble);
                    steps.clear();
                }
            }
            Some("tool") => {
                let id = m["tool_call_id"].as_str().unwrap_or("");
                if let Some(pos) = pending.iter().position(|p| p.0 == id) {
                    let (_, tool, label) = pending.remove(pos);
                    let ok = result_ok(m["content"].as_str().unwrap_or(""));
                    steps.push(json!({"tool": tool, "label": label, "ok": ok}));
                }
                if let Some(f) = followups_from(std::slice::from_ref(m)) {
                    followups = Some(f);
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_mail::FakeMail;
    use crate::mail::MailStore;
    use crate::store::Store;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Seen {
        path: String,
        auth: Option<String>,
        /// anthropic-version, anthropic-beta
        headers: [Option<String>; 2],
        body: Value,
    }

    /// Minimal fake LLM server: answers GETs with a model list and POSTs from a script.
    struct FakeLlm {
        url: String,
        script: Arc<Mutex<VecDeque<(u16, Value)>>>,
        seen: Arc<Mutex<Vec<Seen>>>,
    }

    impl FakeLlm {
        fn start(models: Value) -> Self {
            let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
            let url = format!("http://127.0.0.1:{}", server.server_addr().to_ip().unwrap().port());
            let script: Arc<Mutex<VecDeque<(u16, Value)>>> = Arc::default();
            let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
            let (sc, se) = (script.clone(), seen.clone());
            std::thread::spawn(move || {
                for mut req in server.incoming_requests() {
                    let mut body = String::new();
                    let _ = req.as_reader().read_to_string(&mut body);
                    let header = |name: &'static str| req.headers().iter().find(|h| h.field.equiv(name)).map(|h| h.value.to_string());
                    // Bearer token (OpenAI-style) or x-api-key (Anthropic), recorded the same way.
                    let auth = header("Authorization").or_else(|| header("x-api-key").map(|k| format!("Bearer {k}")));
                    let extra = [header("anthropic-version"), header("anthropic-beta")];
                    let (code, resp) = if *req.method() == tiny_http::Method::Get {
                        if req.url().ends_with("/models") && auth.as_deref() != Some("Bearer test-key-123456") {
                            (401, json!({"error": "bad key"}))
                        } else {
                            (200, models.clone())
                        }
                    } else {
                        se.lock().unwrap().push(Seen { path: req.url().into(), auth, headers: extra.clone(),
                                                       body: serde_json::from_str(&body).unwrap_or_default() });
                        sc.lock().unwrap().pop_front().unwrap_or((500, json!({"error": "script empty"})))
                    };
                    let header = tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap();
                    let _ = req.respond(tiny_http::Response::from_string(resp.to_string()).with_status_code(code).with_header(header));
                }
            });
            FakeLlm { url, script, seen }
        }

        fn push(&self, code: u16, v: Value) {
            self.script.lock().unwrap().push_back((code, v));
        }

        fn take_seen(&self) -> Vec<Seen> {
            std::mem::take(&mut *self.seen.lock().unwrap())
        }
    }

    fn ollama_msg(content: &str, calls: Value) -> Value {
        json!({"message": {"role": "assistant", "content": content, "tool_calls": calls}})
    }

    fn openai_msg(msg: Value) -> Value {
        json!({"choices": [{"index": 0, "message": msg, "finish_reason": "stop"}]})
    }

    #[test]
    fn full_flow_across_providers() {
        let ollama = FakeLlm::start(json!({"models": [{"name": "qwen3:8b"}]}));
        let cloud = FakeLlm::start(json!({"data": [{"id": "models/gemini-2.5-flash"}, {"id": "kimi-k2-0905-preview"}]}));
        let store = Arc::new(Store::in_memory());
        let fake = Arc::new(FakeMail::seeded());
        let settings = Arc::new(Settings::load(store.clone()));
        settings.update(&json!({"providers": {"ollama": {"url": ollama.url}}, "user_name": "Adel"})).unwrap();
        let agent = ChatAgent::new(MailTools::new(fake.clone(), store.clone()), settings.clone());
        assert_eq!(agent.health()["model_installed"], true, "health sees model");

        // -- Ollama turn: list -> read thread (string args) -> draft -> answer with <think>
        ollama.push(200, ollama_msg("", json!([{"function": {"name": "list_emails", "arguments": {"unread_only": true}}}])));
        ollama.push(200, ollama_msg("", json!([{"function": {"name": "read_thread", "arguments": "{\"id\": \"m2\"}"}}])));
        ollama.push(200, ollama_msg("", json!([{"function": {"name": "create_reply_draft",
            "arguments": {"id": "m2", "body": "وعليكم السلام،\nالأربعاء مناسب.\nعادل"}}}])));
        ollama.push(200, ollama_msg("<think>done</think>Saved a reply to [m2] in Drafts.", Value::Null));
        let conv = store.create_conversation("t").unwrap();
        let r = agent.run_turn(&[], "Reply to Khalid saying Wednesday works", Some(&conv), None).unwrap();
        assert_eq!(r.reply, "Saved a reply to [m2] in Drafts.", "think stripped");
        assert_eq!(r.steps.iter().map(|s| s.tool.as_str()).collect::<Vec<_>>(), ["list_emails", "read_thread", "create_reply_draft"]);
        assert!(r.steps.iter().all(|s| s.ok), "tools ok");
        assert_eq!(r.provider, "ollama");
        let seen = ollama.take_seen();
        assert_eq!(seen[0].body["options"]["num_ctx"], 16384, "num_ctx raised");
        assert!(seen[0].body["messages"][0]["content"].as_str().unwrap().contains("Adel"), "user name in prompt");
        let last = seen.last().unwrap().body["messages"].as_array().unwrap().last().unwrap().clone();
        assert_eq!(last["role"], "tool");
        assert_eq!(last["tool_name"], "create_reply_draft");
        assert!(last.get("tool_call_id").is_none(), "no OpenAI fields for Ollama");
        assert!(fake.drafts().iter().any(|d| d.html.contains("الأربعاء مناسب") && d.html.contains(r#"dir="auto""#)));
        store.append_messages(&conv, &r.new_messages).unwrap();

        // -- switch to Gemini, same conversation
        let key = "test-key-123456";
        settings.update(&json!({"provider": "gemini", "temperature": 0.3,
            "providers": {"gemini": {"url": cloud.url, "api_key": key, "use_proxy": false},
                          "kimi": {"url": cloud.url, "use_proxy": false}}})).unwrap();
        let h = agent.health();
        assert_eq!(h["ok"], true);
        assert_eq!(h["model_installed"], true, "models/ prefix stripped");
        let sig = json!({"google": {"thought_signature": "sig-abc"}});
        cloud.push(200, openai_msg(json!({"role": "assistant", "content": null, "tool_calls": [{"id": "call_a", "type": "function",
            "extra_content": sig, "function": {"name": "search_emails", "arguments": "{\"query\": \"budget\"}"}}]})));
        cloud.push(200, openai_msg(json!({"role": "assistant", "content": "Found one: [m3]."})));
        let history = store.load_history(&conv).unwrap();
        let r = agent.run_turn(&history, "Any budget emails?", Some(&conv), Some("m2")).unwrap();
        assert_eq!(r.reply, "Found one: [m3].");
        assert_eq!(r.provider, "gemini");
        let seen = cloud.take_seen();
        assert!(seen[0].body["messages"][0]["content"].as_str().unwrap().contains("email [m2]") ,
                "viewed email passed to the model");
        assert_eq!(seen[0].path, "/chat/completions");
        assert_eq!(seen[0].auth.as_deref(), Some("Bearer test-key-123456"));
        assert_eq!(seen[0].body["temperature"], 0.3);
        let specs: Vec<&Value> = seen[0].body["tools"].as_array().unwrap().iter().map(|t| &t["function"]).collect();
        let lf = specs.iter().find(|f| f["name"] == "list_folders").unwrap();
        assert!(lf.get("parameters").is_none(), "empty schema dropped for Gemini");
        let wire = seen[0].body["messages"].as_array().unwrap();
        let ids: Vec<&Value> = wire.iter().filter_map(|m| m["tool_calls"].as_array()).flatten().map(|c| &c["id"]).collect();
        let tool_msgs: Vec<&Value> = wire.iter().filter(|m| m["role"] == "tool").collect();
        assert_eq!(tool_msgs.len(), 3);
        assert!(tool_msgs.iter().all(|t| ids.contains(&&t["tool_call_id"])), "Ollama turn converted with matching ids");
        assert!(wire.iter().filter_map(|m| m["tool_calls"].as_array()).flatten().all(|c| c["function"]["arguments"].is_string()));
        let wire2 = seen[1].body["messages"].as_array().unwrap();
        let echo = wire2.iter().filter_map(|m| m["tool_calls"].as_array()).flatten().find(|c| c["id"] == "call_a").unwrap();
        assert_eq!(echo["extra_content"], sig, "thought signature echoed");
        assert_eq!(wire2.last().unwrap()["tool_call_id"], "call_a");
        store.append_messages(&conv, &r.new_messages).unwrap();

        // -- error: nothing is appended
        cloud.push(401, json!({"error": {"message": "API key not valid"}}));
        let history = store.load_history(&conv).unwrap();
        let e = agent.run_turn(&history, "again", Some(&conv), None).unwrap_err();
        assert!(e.0.contains("401") && e.0.contains("API key rejected"), "{}", e.0);
        cloud.take_seen();

        // -- Kimi: reasoning_content passed back, temperature clamped, empty-string args
        settings.update(&json!({"provider": "kimi", "temperature": 1.5, "providers": {"kimi": {"api_key": key}}})).unwrap();
        cloud.push(200, openai_msg(json!({"role": "assistant", "content": "", "reasoning_content": "need folders",
            "tool_calls": [{"id": "list_folders:0", "type": "function", "function": {"name": "list_folders", "arguments": ""}}]})));
        cloud.push(200, openai_msg(json!({"role": "assistant", "content": "You have Inbox/Projects."})));
        let r = agent.run_turn(&history, "What folders do I have?", Some(&conv), None).unwrap();
        assert!(r.steps[0].ok, "empty-string args accepted");
        let seen = cloud.take_seen();
        assert_eq!(seen[0].body["temperature"], 1.0, "Kimi clamp");
        let k2 = seen[1].body["messages"].as_array().unwrap();
        assert!(k2.iter().any(|m| m["reasoning_content"] == "need folders"));
        assert!(k2.iter().filter(|m| m["role"] == "assistant").all(|m| !m["content"].is_null() || m.get("tool_calls").is_some()));
        store.append_messages(&conv, &r.new_messages).unwrap();

        // -- back to Ollama with cloud-format history
        settings.update(&json!({"provider": "ollama"})).unwrap();
        ollama.push(200, ollama_msg("ok", Value::Null));
        let r = agent.run_turn(&store.load_history(&conv).unwrap(), "thanks", Some(&conv), None).unwrap();
        assert_eq!(r.reply, "ok");
        assert!(ollama.take_seen()[0].body["messages"].as_array().unwrap().iter().all(|m| m.get("tool_call_id").is_none()));
        store.append_messages(&conv, &r.new_messages).unwrap();

        // -- the conversation survives and renders
        let shown = display_history(&store.load_history(&conv).unwrap());
        let roles: Vec<&str> = shown.iter().map(|m| m["role"].as_str().unwrap()).collect();
        assert_eq!(roles, ["user", "assistant", "user", "assistant", "user", "assistant", "user", "assistant"]);
        assert_eq!(shown[1]["steps"].as_array().unwrap().len(), 3, "steps rebuilt from history");
        assert_eq!(shown[1]["steps"][2]["label"], "Saving reply draft to m2");
    }

    #[test]
    fn anthropic_and_openai_turns() {
        let claude = FakeLlm::start(json!({"data": [{"id": "claude-opus-5-5", "display_name": "Claude Opus 5.5"}]}));
        let openai = FakeLlm::start(json!({"data": [{"id": "gpt-5.1"}, {"id": "gpt-4o-mini-tts"}, {"id": "text-embedding-3-small"}, {"id": "o4-mini"}]}));
        let store = Arc::new(Store::in_memory());
        let fake = Arc::new(FakeMail::seeded());
        let settings = Arc::new(Settings::load(store.clone()));
        let key = "test-key-123456";
        settings.update(&json!({"provider": "anthropic", "providers": {
            "anthropic": {"url": claude.url, "api_key": key, "use_proxy": false, "model": "claude-opus-5-5"},
            "openai": {"url": openai.url, "api_key": key, "use_proxy": false, "model": "gpt-5.1"}}})).unwrap();
        let agent = ChatAgent::new(MailTools::new(fake.clone(), store.clone()), settings.clone());
        let h = agent.health();
        assert_eq!(h["model_installed"], true, "{h}");
        assert_eq!(h["model_labels"]["claude-opus-5-5"], "Claude Opus 5.5 · claude-opus-5-5");

        // -- Claude: (thinking + tool call), then the answer
        let thinking = json!({"type": "thinking", "thinking": "", "signature": "sig-1"});
        claude.push(200, json!({"content": [thinking, {"type": "tool_use", "id": "toolu_1", "name": "list_emails",
                                "input": {"unread_only": true}}], "stop_reason": "tool_use"}));
        claude.push(200, json!({"content": [{"type": "text", "text": "Two unread: [m1], [m2]."}], "stop_reason": "end_turn"}));
        let conv = store.create_conversation("t").unwrap();
        let r = agent.run_turn(&[], "What's unread?", Some(&conv), None).unwrap();
        assert_eq!(r.reply, "Two unread: [m1], [m2].");
        assert_eq!(r.steps[0].tool, "list_emails");
        let seen = claude.take_seen();
        assert_eq!(seen[0].path, "/messages");
        assert_eq!(seen[0].auth.as_deref(), Some("Bearer test-key-123456"), "sent as x-api-key");
        assert_eq!(seen[0].headers[0].as_deref(), Some(crate::llm::ANTHROPIC_VERSION));
        assert!(seen[0].body.get("temperature").is_none(), "no sampling parameters on this API version");
        assert!(seen[0].body["max_tokens"].as_u64().unwrap() > 0);
        assert!(seen[0].body["system"].as_str().unwrap().contains("private email assistant"));
        assert_eq!(seen[0].body["tools"][0]["input_schema"]["type"], "object");
        assert!(seen[0].body.get("fallbacks").is_none() && seen[0].headers[1].is_none(), "server fallbacks only on api.anthropic.com");
        let m2 = seen[1].body["messages"].as_array().unwrap();
        assert_eq!(m2.len(), 3);
        assert_eq!(m2[1]["content"][0], thinking, "thinking block sent back unchanged within the turn");
        assert_eq!((m2[2]["content"][0]["type"].as_str(), m2[2]["content"][0]["tool_use_id"].as_str()), (Some("tool_result"), Some("toolu_1")));
        assert_eq!(seen[0].body["system"], seen[1].body["system"], "system prompt fixed for the whole turn");
        assert_eq!(seen[0].body["messages"][0], m2[0], "requests within a turn only append");
        store.append_messages(&conv, &r.new_messages).unwrap();

        // -- next turn: earlier thinking is not replayed
        claude.push(200, json!({"content": [{"type": "text", "text": "ok"}], "stop_reason": "end_turn"}));
        let r = agent.run_turn(&store.load_history(&conv).unwrap(), "thanks", Some(&conv), None).unwrap();
        let seen = claude.take_seen();
        let msgs = seen[0].body["messages"].as_array().unwrap();
        assert!(!msgs.iter().flat_map(|m| m["content"].as_array().cloned().unwrap_or_default()).any(|b| b["type"] == "thinking"));
        assert_eq!(msgs.iter().map(|m| m["role"].as_str().unwrap()).collect::<Vec<_>>(), ["user", "assistant", "user", "assistant", "user"]);
        store.append_messages(&conv, &r.new_messages).unwrap();

        // -- a declined request is an error and stores nothing
        claude.push(200, json!({"content": [], "stop_reason": "refusal", "stop_details": {"type": "refusal", "category": "cyber"}}));
        let e = agent.run_turn(&store.load_history(&conv).unwrap(), "x", Some(&conv), None).unwrap_err();
        assert!(e.0.contains("declined") && e.0.contains("cyber"), "{}", e.0);
        claude.take_seen();

        // -- ChatGPT: chat models only; GPT-5 models get no temperature; tool messages without "name"
        settings.update(&json!({"provider": "openai"})).unwrap();
        assert_eq!(agent.health()["models"], json!(["gpt-5.1", "o4-mini"]));
        openai.push(200, openai_msg(json!({"role": "assistant", "content": null, "tool_calls": [{"id": "call_x", "type": "function",
            "function": {"name": "list_folders", "arguments": "{}"}}]})));
        openai.push(200, openai_msg(json!({"role": "assistant", "content": "Inbox and Projects."})));
        let r = agent.run_turn(&store.load_history(&conv).unwrap(), "folders?", Some(&conv), None).unwrap();
        assert_eq!(r.reply, "Inbox and Projects.");
        let seen = openai.take_seen();
        assert_eq!(seen[0].path, "/chat/completions");
        assert!(seen[0].body.get("temperature").is_none(), "gpt-5.1 accepts only its default temperature");
        let wire = seen[1].body["messages"].as_array().unwrap();
        assert!(wire.iter().filter(|m| m["role"] == "tool").all(|m| m.get("name").is_none() && m["tool_call_id"].is_string()));
        assert!(wire.iter().all(|m| m.get("raw").is_none()), "Claude's blocks never go to other providers");
        settings.update(&json!({"providers": {"openai": {"model": "gpt-4.1"}}})).unwrap();
        openai.push(200, openai_msg(json!({"role": "assistant", "content": "ok"})));
        agent.run_turn(&[], "hi", None, None).unwrap();
        assert_eq!(openai.take_seen()[0].body["temperature"], 0.2, "older models keep the temperature setting");
    }

    #[test]
    fn insights_panel() {
        let ollama = FakeLlm::start(json!({"models": [{"name": "qwen3:8b"}]}));
        let store = Arc::new(Store::in_memory());
        let fake = Arc::new(FakeMail::demo());
        let settings = Arc::new(Settings::load(store.clone()));
        settings.update(&json!({"providers": {"ollama": {"url": ollama.url}}})).unwrap();
        let agent = ChatAgent::new(MailTools::new(fake.clone(), store.clone()), settings);
        agent.tools.ctx.save(crate::fake_mail::sample_profile()).unwrap();
        let p = agent.tools.ctx.profile();
        crate::learn::learn(fake.as_ref(), &store, &p.style, &p.learning, &Default::default()).unwrap();
        let rows = fake.list_messages("inbox", Some(7), true, 5).unwrap();
        let khalid = rows.iter().find(|s| s.from_name == "Khalid Mansour").unwrap().entry_id.clone();

        ollama.push(200, ollama_msg("```json\n{\"summary\": \"خالد يطلب اجتماعاً يوم الأربعاء.\", \"actions\": [\"تأكيد موعد الاجتماع\"], \
            \"needs_reply\": true, \"greeting_name\": \"الأستاذ خالد\", \"reply\": \"يناسبني الأربعاء.\"}\n```", Value::Null));
        let i = agent.insights(&khalid, false).unwrap();
        assert_eq!(i.summary, "خالد يطلب اجتماعاً يوم الأربعاء.");
        assert_eq!(i.actions, ["تأكيد موعد الاجتماع"]);
        assert!(i.needs_reply && i.reply == "يناسبني الأربعاء." && i.greeting_name == "الأستاذ خالد" && !i.cached);
        let seen = ollama.take_seen();
        assert_eq!(seen[0].body["format"], "json", "JSON mode for Ollama");
        assert!(seen[0].body.get("tools").is_none(), "no tools for the panel");
        let sys = seen[0].body["messages"][0]["content"].as_str().unwrap();
        assert!(sys.contains("الخدمات المتميزة") && sys.contains("Past replies by the user") && sys.contains("مع تحياتي"),
                "profile, learned examples and style in the prompt");
        assert!(sys.contains("2–4 sentences in Arabic") && sys.contains("Write drafts in Arabic"), "the profile's language");
        assert!(seen[0].body["messages"][1]["content"].as_str().unwrap().contains("نرغب في عقد اجتماع"));

        let again = agent.insights(&khalid, false).unwrap();
        assert!(again.cached && again.summary == i.summary && ollama.take_seen().is_empty(), "served from cache");

        let p = parse_insights("Sorry, here it is: {\"summary\": \"x\", \"actions\": \"- a\\n- b\", \"reply\": \"r\"}");
        assert_eq!((p.actions.len(), p.needs_reply), (2, true), "lenient parsing");
        assert_eq!(parse_insights("plain text").summary, "plain text");
    }

    #[test]
    fn open_email_is_the_default_context() {
        let ollama = FakeLlm::start(json!({"models": [{"name": "qwen3:8b"}]}));
        let store = Arc::new(Store::in_memory());
        let fake = Arc::new(FakeMail::seeded());
        let settings = Arc::new(Settings::load(store.clone()));
        settings.update(&json!({"providers": {"ollama": {"url": ollama.url}}})).unwrap();
        let agent = ChatAgent::new(MailTools::new(fake.clone(), store.clone()), settings);
        let khalid = fake.list_messages("inbox", Some(7), false, 9).unwrap().into_iter()
            .find(|s| s.from_name == "Khalid Mansour").unwrap().entry_id;
        let open = store.short_id(&khalid).unwrap();

        // The model doesn't pass an id: the open email is used.
        ollama.push(200, ollama_msg("", json!([{"function": {"name": "create_reply_draft", "arguments": {"body": "يناسبني الأربعاء."}}}])));
        ollama.push(200, ollama_msg("تم حفظ الرد في المسودات.", Value::Null));
        let r = agent.run_turn(&[], "رد بأن الأربعاء مناسب", None, Some(&open)).unwrap();
        assert!(r.steps[0].ok, "reply drafted without an explicit id");
        assert_eq!(fake.drafts().last().unwrap().subject, "RE: طلب اجتماع بخصوص ميزانية ٢٠٢٧", "reply went to the open email");
        let seen = ollama.take_seen();
        let msgs = seen[0].body["messages"].as_array().unwrap();
        let sys = msgs[0]["content"].as_str().unwrap();
        assert!(sys.contains("OPEN EMAIL") && sys.contains(&format!("use id \"{open}\"")) && sys.contains("نرغب في عقد اجتماع"),
                "open email's content is in the context");
        assert!(msgs.last().unwrap()["content"].as_str().unwrap().starts_with(&format!("[About open email [{open}]")),
                "each user message is anchored to the email it was about");
        assert_eq!(r.new_messages[0]["viewing"], json!(open));
        assert_eq!(display_history(&r.new_messages)[0]["viewing"], json!(open));

        // "this" also means the open email; an explicit other id is respected.
        let t = &agent.tools;
        let this = t.call_with("read_email", &json!({"id": "this"}), None, Some(&open));
        assert!(this.contains("Khalid Mansour"));
        let other = t.call_with("read_email", &json!({"id": "m999"}), None, Some(&open));
        assert!(other.contains("Unknown email id"), "an explicit id is never overridden");
    }

    #[test]
    fn compact_truncates_old_tool_output_only() {
        let big = "x".repeat(2000);
        let h = vec![
            json!({"role": "tool", "content": "orphan"}),
            json!({"role": "user", "content": "a"}),
            json!({"role": "tool", "content": big}),
            json!({"role": "user", "content": "b"}),
            json!({"role": "tool", "content": big}),
        ];
        let c = compact(&h, 40);
        assert_eq!(c[0]["role"], "user", "starts at a user message");
        assert!(c[1]["content"].as_str().unwrap().ends_with("[older result truncated]\""));
        assert_eq!(c[3]["content"].as_str().unwrap().len(), 2000, "current turn untouched");
    }
}
