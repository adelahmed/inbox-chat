//! LLM providers: Ollama (native /api/chat), Anthropic Claude (native Messages API) and OpenAI ChatGPT,
//! Google Gemini and Moonshot Kimi (OpenAI-compatible /chat/completions).
//!
//! History is stored in one neutral format and converted per provider, so the provider can change
//! mid-conversation:
//!   {"role": "user", "content"}
//!   {"role": "assistant", "content", "tool_calls"?: [{"id", "name", "arguments": {..}, "extra"?}], "reasoning"?,
//!    "raw"?: {"provider", "content"}}
//!   {"role": "tool", "content", "tool_call_id", "tool_name"}
//! `raw` keeps a provider's own reply blocks (Claude's thinking blocks) to send back unchanged within the
//! same turn; earlier turns are rebuilt from the neutral fields.

use crate::settings::{provider_info, Config};
use serde_json::{json, Map, Value};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct LlmError(pub String);

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Default)]
pub struct ToolCall {
    pub id: Option<String>,
    pub name: String,
    /// A JSON object, or a JSON string some providers send instead.
    pub arguments: Value,
    pub extra: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Default)]
pub struct Completion {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub reasoning: Option<String>,
    /// The provider's own reply blocks, to echo back unchanged within the turn (Anthropic).
    pub raw: Option<Value>,
}

/// Anthropic API version sent with every request.
pub const ANTHROPIC_VERSION: &str = "2026-10-21";

/// Claude models that take server-side refusal fallbacks (`fallbacks: "default"`) on the Claude API.
const ANTHROPIC_FALLBACK_MODELS: &[&str] = &["claude-fable-5-1", "claude-opus-5-5", "claude-opus-5", "claude-sonnet-5-5"];

pub struct Backend {
    pub provider: String,
    pub label: &'static str,
    pub local: bool,
    pub url: String,
    pub model: String,
    api_key: String,
    cfg: Config,
    client: reqwest::blocking::Client,
}

fn host_of(url: &str) -> &str {
    url.split('/').nth(2).unwrap_or(url)
}

/// Chat-capable models from an Ollama /api/tags response, as (name, label), sorted by name.
/// Models whose `capabilities` lack "completion" (embedding models) are left out; older Ollama
/// versions don't report capabilities, so then every model is kept.
pub fn ollama_chat_models(tags: &Value) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = tags["models"].as_array().into_iter().flatten().filter_map(|m| {
        let name = m["name"].as_str()?.to_string();
        let caps: Option<Vec<&str>> = m["capabilities"].as_array().map(|c| c.iter().filter_map(Value::as_str).collect());
        if caps.as_ref().is_some_and(|c| !c.contains(&"completion")) {
            return None;
        }
        let mut label = name.clone();
        if let Some(size) = m["details"]["parameter_size"].as_str() {
            label.push_str(&format!(" · {size}"));
        }
        if caps.as_ref().is_some_and(|c| !c.contains(&"tools")) {
            label.push_str(" · no tools");
        }
        Some((name, label))
    }).collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ollama_server_models() {
        // A real /api/tags answer from a multi-model Ollama server.
        let tags: Value = serde_json::from_str(include_str!("../tests/fixtures/ollama_tags.json")).unwrap();
        let models = ollama_chat_models(&tags);
        let names: Vec<&str> = models.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["gemma4:31b", "qwen3-coder:30b", "qwen3-vl:32b", "qwen3.6:27b", "qwen3.6:35b", "qwen3.8:27b"],
                   "embedding-only models are hidden");
        assert!(models.contains(&("qwen3.8:27b".into(), "qwen3.8:27b · 27.3B".into())));
        assert!(ollama_chat_models(&json!({"models": [{"name": "old:7b"}]})).len() == 1, "no capabilities reported → kept");
        assert_eq!(ollama_chat_models(&json!({"models": [{"name": "x", "capabilities": ["completion"]}]}))[0].1, "x · no tools");
    }

    #[test]
    fn anthropic_wire_format() {
        let h = vec![
            json!({"role": "user", "content": "folders?"}),
            json!({"role": "assistant", "content": "", "reasoning": "r",
                   "tool_calls": [{"id": "list_folders:0", "name": "list_folders", "arguments": "{}"},
                                  {"id": "call_2", "name": "list_emails", "arguments": {"days": 1}}],
                   "raw": {"provider": "anthropic", "content": [{"type": "thinking", "thinking": "", "signature": "s"}]}}),
            json!({"role": "tool", "content": "{}", "tool_call_id": "list_folders:0", "tool_name": "list_folders"}),
            json!({"role": "tool", "content": "", "tool_call_id": "call_2", "tool_name": "list_emails"}),
            json!({"role": "assistant", "content": "Done."}),
            json!({"role": "assistant", "content": ""}),
            json!({"role": "user", "content": "and?", "viewing": "m3", "viewing_subject": "Budget"}),
        ];
        let w = Backend::anthropic_wire(&h);
        let roles: Vec<&str> = w.iter().map(|m| m["role"].as_str().unwrap()).collect();
        assert_eq!(roles, ["user", "assistant", "user", "assistant", "user"], "empty turns dropped, roles alternate");
        assert_eq!(w[1]["content"][0]["id"], "list_folders_0", "ids made valid for Anthropic");
        assert_eq!(w[1]["content"][0]["input"], json!({}), "string arguments parsed");
        assert_eq!(w[1]["content"][1]["input"], json!({"days": 1}));
        assert!(w[1]["content"].as_array().unwrap().iter().all(|b| b["type"] != "thinking"), "earlier turns rebuilt without thinking");
        assert_eq!(w[2]["content"].as_array().unwrap().len(), 2, "tool results of one step in one user message");
        assert_eq!(w[2]["content"][0]["tool_use_id"], "list_folders_0");
        assert_eq!(w[2]["content"][1]["content"], "(empty)");
        assert!(w[4]["content"][0]["text"].as_str().unwrap().starts_with("[About open email [m3] «Budget»]"));
        let tools = anthropic_tools(&[json!({"type": "function", "function": {"name": "a", "description": "d",
                                                                              "parameters": {"type": "object", "properties": {}}}})]);
        assert_eq!(tools[0], json!({"name": "a", "description": "d", "input_schema": {"type": "object", "properties": {}}}));
    }

    #[test]
    fn openai_model_rules() {
        assert!(openai_fixed_temperature("gpt-5.1") && openai_fixed_temperature("o3") && openai_fixed_temperature("o4-mini"));
        assert!(!openai_fixed_temperature("gpt-4.1") && !openai_fixed_temperature("omni-x"));
        assert!(openai_chat_model("gpt-4.1-mini") && openai_chat_model("o3") && openai_chat_model("chatgpt-4o-latest"));
        assert!(!openai_chat_model("gpt-image-1") && !openai_chat_model("whisper-1") && !openai_chat_model("text-embedding-3-large")
                && !openai_chat_model("gpt-4o-realtime-preview") && !openai_chat_model("dall-e-3"));
    }
}

/// User text as sent to the model: prefixed with the email that was open when it was written.
fn user_text(m: &Value) -> Value {
    match (m["viewing"].as_str(), m["content"].as_str()) {
        (Some(id), Some(text)) => json!(format!("[About open email [{id}] «{}»]\n{text}", m["viewing_subject"].as_str().unwrap_or(""))),
        _ => m["content"].clone(),
    }
}

fn last_user(history: &[Value]) -> usize {
    history.iter().rposition(|m| m["role"] == "user").unwrap_or(0)
}

/// OpenAI reasoning models (GPT-5 family, o-series) accept only the default temperature.
fn openai_fixed_temperature(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    m.starts_with("gpt-5") || (m.starts_with('o') && m[1..].starts_with(|c: char| c.is_ascii_digit()))
}

/// Chat models from OpenAI's /models list (it also lists audio, image, embedding and moderation models).
fn openai_chat_model(id: &str) -> bool {
    let m = id.to_ascii_lowercase();
    let chat = m.starts_with("gpt-") || m.starts_with("chatgpt-") || (m.starts_with('o') && m[1..].starts_with(|c: char| c.is_ascii_digit()));
    let other = ["audio", "realtime", "tts", "transcribe", "image", "embedding", "moderation", "instruct", "search", "dall-e", "whisper", "codex"];
    chat && !other.iter().any(|o| m.contains(o))
}

/// Anthropic tool ids must match ^[a-zA-Z0-9_-]+$; ids from other providers may not.
fn anthropic_id(id: &Value) -> String {
    let s: String = id.as_str().unwrap_or("").chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect();
    if s.is_empty() { "call".into() } else { s }
}

fn args_object(v: &Value) -> Value {
    match v {
        Value::Object(_) => v.clone(),
        Value::String(s) => serde_json::from_str::<Value>(s).ok().filter(Value::is_object).unwrap_or_else(|| json!({})),
        _ => json!({}),
    }
}

/// Neutral tool specs (OpenAI style) as Anthropic tools.
fn anthropic_tools(specs: &[Value]) -> Vec<Value> {
    specs.iter().map(|s| {
        let f = &s["function"];
        let mut schema = f["parameters"].clone();
        if !schema.is_object() {
            schema = json!({"type": "object", "properties": {}});
        }
        json!({"name": f["name"], "description": f["description"], "input_schema": schema})
    }).collect()
}

/// Gemini rejects OBJECT schemas with no properties; drop empty parameter blocks.
fn openai_tools(specs: &[Value]) -> Vec<Value> {
    specs.iter().map(|s| {
        let mut f = s["function"].clone();
        let params = f["parameters"].clone();
        let empty_props = params["properties"].as_object().map(|p| p.is_empty()).unwrap_or(true);
        if empty_props {
            f.as_object_mut().unwrap().remove("parameters");
        } else if params["required"].as_array().map(|r| r.is_empty()).unwrap_or(true) {
            f["parameters"].as_object_mut().unwrap().remove("required");
        }
        json!({"type": "function", "function": f})
    }).collect()
}

impl Backend {
    pub fn new(cfg: &Config) -> Result<Self, LlmError> {
        let info = provider_info(&cfg.provider).ok_or_else(|| LlmError(format!("Unknown provider {}", cfg.provider)))?;
        let p = cfg.active();
        let mut b = reqwest::blocking::Client::builder().connect_timeout(Duration::from_secs(10));
        if !p.use_proxy {
            b = b.no_proxy(); // local/intranet Ollama: bypass any corporate proxy
        } else if !p.proxy_url.is_empty() {
            let proxy = reqwest::Proxy::all(&p.proxy_url).map_err(|e| LlmError(format!("Invalid proxy URL: {e}")))?;
            b = b.proxy(proxy);
        } // else: system proxy settings / HTTP(S)_PROXY
        if let Ok(ca) = std::env::var("INBOX_CHAT_CA_BUNDLE") {
            let pem = std::fs::read(&ca).map_err(|e| LlmError(format!("Cannot read INBOX_CHAT_CA_BUNDLE: {e}")))?;
            for cert in reqwest::Certificate::from_pem_bundle(&pem).map_err(|e| LlmError(format!("Bad CA bundle: {e}")))? {
                b = b.add_root_certificate(cert);
            }
        }
        Ok(Backend {
            provider: cfg.provider.clone(),
            label: info.label,
            local: info.local,
            url: p.url.trim_end_matches('/').to_string(),
            model: p.model.clone(),
            api_key: p.api_key.clone(),
            cfg: cfg.clone(),
            client: b.build().map_err(|e| LlmError(format!("HTTP client: {e}")))?,
        })
    }

    fn is_ollama(&self) -> bool {
        self.provider == "ollama"
    }

    fn send(&self, req: reqwest::blocking::RequestBuilder, timeout: u64) -> Result<Value, LlmError> {
        let who = self.label;
        let resp = req.timeout(Duration::from_secs(timeout)).send().map_err(|e| {
            if e.is_timeout() {
                LlmError(format!("{who} did not answer within {timeout}s."))
            } else {
                // Walk the source chain for the useful part (DNS, refused, TLS...).
                let mut msg = e.to_string();
                let mut src = std::error::Error::source(&e);
                while let Some(s) = src {
                    msg = s.to_string();
                    src = s.source();
                }
                LlmError(format!("Cannot reach {who} at {} ({msg}).", host_of(&self.url)))
            }
        })?;
        let status = resp.status();
        let text = resp.text().unwrap_or_default();
        if !status.is_success() {
            let hint = match status.as_u16() {
                401 => " (API key rejected)",
                403 => " (access denied)",
                429 => " (rate limit or quota reached)",
                529 => " (the service is overloaded; try again shortly)",
                _ => "",
            };
            let detail: String = text.chars().take(500).collect();
            return Err(LlmError(format!("{who} returned HTTP {}{hint}: {detail}", status.as_u16())));
        }
        serde_json::from_str(&text).map_err(|e| LlmError(format!("{who} sent an unreadable answer: {e}")))
    }

    fn is_anthropic(&self) -> bool {
        self.provider == "anthropic"
    }

    fn key(&self) -> Result<&str, LlmError> {
        if self.api_key.is_empty() {
            return Err(LlmError(format!("No API key set for {}. Open Settings and add one.", self.label)));
        }
        Ok(&self.api_key)
    }

    fn auth(&self) -> Result<String, LlmError> {
        Ok(format!("Bearer {}", self.key()?))
    }

    fn anthropic_get(&self, path: &str) -> Result<reqwest::blocking::RequestBuilder, LlmError> {
        Ok(self.client.get(format!("{}{path}", self.url)).header("x-api-key", self.key()?)
            .header("anthropic-version", ANTHROPIC_VERSION))
    }

    // -- wire formats ---------------------------------------------------------------
    fn ollama_wire(history: &[Value]) -> Vec<Value> {
        history.iter().map(|m| match m["role"].as_str() {
            Some("assistant") => {
                let mut w = json!({"role": "assistant", "content": m["content"].as_str().unwrap_or("")});
                if let Some(calls) = m["tool_calls"].as_array().filter(|c| !c.is_empty()) {
                    w["tool_calls"] = calls.iter()
                        .map(|c| json!({"function": {"name": c["name"], "arguments": c["arguments"]}}))
                        .collect();
                }
                w
            }
            Some("tool") => json!({"role": "tool", "content": m["content"], "tool_name": m["tool_name"]}),
            _ => json!({"role": m["role"], "content": user_text(m)}),
        }).collect()
    }

    fn openai_wire(history: &[Value]) -> Vec<Value> {
        let lu = last_user(history);
        let mut out = Vec::new();
        for (i, m) in history.iter().enumerate() {
            match m["role"].as_str() {
                Some("assistant") => {
                    let content = m["content"].as_str().unwrap_or("");
                    let calls = m["tool_calls"].as_array().cloned().unwrap_or_default();
                    if content.is_empty() && calls.is_empty() {
                        continue; // some providers reject empty assistant turns
                    }
                    let mut w = json!({"role": "assistant", "content": if content.is_empty() { Value::Null } else { json!(content) }});
                    if !calls.is_empty() {
                        w["tool_calls"] = calls.iter().map(|c| {
                            let mut tc = json!({
                                "id": c["id"], "type": "function",
                                "function": {"name": c["name"], "arguments": c["arguments"].to_string()},
                            });
                            // e.g. Gemini thought signatures must be echoed back unchanged
                            if let Some(extra) = c["extra"].as_object() {
                                for (k, v) in extra {
                                    tc[k] = v.clone();
                                }
                            }
                            tc
                        }).collect();
                    }
                    if i > lu {
                        if let Some(r) = m["reasoning"].as_str() {
                            w["reasoning_content"] = json!(r); // Kimi thinking models need it within a turn
                        }
                    }
                    out.push(w);
                }
                Some("tool") => out.push(json!({
                    "role": "tool", "tool_call_id": m["tool_call_id"], "name": m["tool_name"], "content": m["content"],
                })),
                _ => out.push(json!({"role": m["role"], "content": user_text(m)})),
            }
        }
        out
    }

    /// Anthropic Messages format. Within the current turn Claude's own blocks are sent back exactly as
    /// received (thinking blocks are bound to the conversation and must not change); earlier turns are
    /// rebuilt from text and tool calls without thinking. Consecutive same-role messages are merged.
    fn anthropic_wire(history: &[Value]) -> Vec<Value> {
        let lu = last_user(history);
        let mut out: Vec<Value> = Vec::new();
        let mut push = |role: &str, blocks: Vec<Value>| {
            if blocks.is_empty() {
                return;
            }
            match out.last_mut().filter(|m| m["role"] == role) {
                Some(last) => last["content"].as_array_mut().expect("blocks").extend(blocks),
                None => out.push(json!({"role": role, "content": blocks})),
            }
        };
        for (i, m) in history.iter().enumerate() {
            match m["role"].as_str() {
                Some("assistant") => {
                    if let (true, Some(raw)) = (i > lu && m["raw"]["provider"] == "anthropic", m["raw"]["content"].as_array()) {
                        push("assistant", raw.clone());
                        continue;
                    }
                    let mut blocks = Vec::new();
                    let text = m["content"].as_str().unwrap_or("");
                    if !text.trim().is_empty() {
                        blocks.push(json!({"type": "text", "text": text}));
                    }
                    for c in m["tool_calls"].as_array().into_iter().flatten() {
                        blocks.push(json!({"type": "tool_use", "id": anthropic_id(&c["id"]), "name": c["name"],
                                           "input": args_object(&c["arguments"])}));
                    }
                    push("assistant", blocks);
                }
                Some("tool") => {
                    let content = m["content"].as_str().filter(|s| !s.is_empty()).unwrap_or("(empty)");
                    push("user", vec![json!({"type": "tool_result", "tool_use_id": anthropic_id(&m["tool_call_id"]), "content": content})]);
                }
                _ => {
                    let text = user_text(m);
                    let text = text.as_str().unwrap_or("");
                    if !text.trim().is_empty() {
                        push("user", vec![json!({"type": "text", "text": text})]);
                    }
                }
            }
        }
        out
    }

    fn anthropic_complete(&self, system: &str, history: &[Value], tools: &[Value]) -> Result<Completion, LlmError> {
        // No temperature: API version 2026-10-21 rejects sampling parameters. Thinking is left to the
        // model's default (adaptive on current models).
        let mut payload = json!({"model": self.model, "max_tokens": 16000, "system": system,
                                 "messages": Self::anthropic_wire(history)});
        if !tools.is_empty() {
            payload["tools"] = json!(anthropic_tools(tools));
        }
        let mut req = self.client.post(format!("{}/messages", self.url)).header("x-api-key", self.key()?)
            .header("anthropic-version", ANTHROPIC_VERSION);
        // A request a model's safety classifier declines is retried server-side on another model.
        if host_of(&self.url) == "api.anthropic.com" && ANTHROPIC_FALLBACK_MODELS.contains(&self.model.as_str()) {
            payload["fallbacks"] = json!("default");
            req = req.header("anthropic-beta", "server-side-fallback-2026-07-01");
        }
        let data = self.send(req.json(&payload), 600)?;
        if data["stop_reason"] == "refusal" {
            let why = data["stop_details"]["category"].as_str().map(|c| format!(" ({c})")).unwrap_or_default();
            return Err(LlmError(format!("Claude declined this request{why}. Rephrase it or choose another model in Settings.")));
        }
        let Some(blocks) = data["content"].as_array() else {
            let s: String = data.to_string().chars().take(300).collect();
            return Err(LlmError(format!("{} returned no answer: {s}", self.label)));
        };
        // After a mid-answer fallback, only what follows the last `fallback` marker is the served answer;
        // model-internal blocks before it are not sent back.
        let start = blocks.iter().rposition(|b| b["type"] == "fallback").map_or(0, |i| i + 1);
        let raw: Vec<Value> = blocks.iter().enumerate()
            .filter(|(i, b)| *i >= start || b["type"] == "text")
            .map(|(_, b)| b.clone()).collect();
        let text: Vec<&str> = blocks.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect();
        let calls = blocks[start..].iter().filter(|b| b["type"] == "tool_use").map(|b| ToolCall {
            id: b["id"].as_str().map(String::from),
            name: b["name"].as_str().unwrap_or("").into(),
            arguments: b["input"].clone(),
            extra: None,
        }).collect();
        Ok(Completion { content: text.join("\n"), tool_calls: calls, reasoning: None, raw: Some(Value::Array(raw)) })
    }

    // -- calls ------------------------------------------------------------------------
    pub fn complete(&self, system: &str, history: &[Value], tools: &[Value]) -> Result<Completion, LlmError> {
        self.complete_with(system, history, tools, false)
    }

    /// `json_mode`: ask Ollama for strict JSON output (other providers are parsed leniently).
    pub fn complete_with(&self, system: &str, history: &[Value], tools: &[Value], json_mode: bool) -> Result<Completion, LlmError> {
        if self.is_ollama() {
            let mut messages = vec![json!({"role": "system", "content": system})];
            messages.extend(Self::ollama_wire(history));
            let mut payload = json!({
                "model": self.model, "messages": messages, "stream": false,
                // Ollama's default context (2-4k) is far too small for email; raise it.
                "options": {"num_ctx": self.cfg.num_ctx, "temperature": self.cfg.temperature},
            });
            if !tools.is_empty() {
                payload["tools"] = json!(tools);
            }
            if json_mode {
                payload["format"] = json!("json");
            }
            if self.cfg.think == "true" || self.cfg.think == "false" {
                payload["think"] = json!(self.cfg.think == "true");
            }
            let data = self.send(self.client.post(format!("{}/api/chat", self.url)).json(&payload), 600)?;
            let msg = &data["message"];
            let calls = msg["tool_calls"].as_array().map(|cs| cs.iter().map(|c| ToolCall {
                id: None,
                name: c["function"]["name"].as_str().unwrap_or("").into(),
                arguments: c["function"]["arguments"].clone(),
                extra: None,
            }).collect()).unwrap_or_default();
            return Ok(Completion { content: msg["content"].as_str().unwrap_or("").into(), tool_calls: calls, ..Default::default() });
        }
        if self.is_anthropic() {
            return self.anthropic_complete(system, history, tools);
        }

        let mut temperature = self.cfg.temperature;
        if self.provider == "kimi" {
            temperature = temperature.min(1.0); // Moonshot accepts 0-1
        }
        let mut messages = vec![json!({"role": "system", "content": system})];
        messages.extend(Self::openai_wire(history));
        if self.provider == "openai" {
            for m in messages.iter_mut().filter(|m| m["role"] == "tool") {
                m.as_object_mut().expect("message").remove("name"); // not part of OpenAI's tool message
            }
        }
        let mut payload = json!({"model": self.model, "messages": messages, "stream": false});
        if !(self.provider == "openai" && openai_fixed_temperature(&self.model)) {
            payload["temperature"] = json!(temperature);
        }
        if !tools.is_empty() {
            payload["tools"] = json!(openai_tools(tools)); // an empty list is rejected by some providers
        }
        let req = self.client.post(format!("{}/chat/completions", self.url))
            .header("Authorization", self.auth()?).json(&payload);
        let data = self.send(req, 300)?;
        let Some(msg) = data["choices"].get(0).map(|c| &c["message"]) else {
            let s: String = data.to_string().chars().take(300).collect();
            return Err(LlmError(format!("{} returned no answer: {s}", self.label)));
        };
        let calls = msg["tool_calls"].as_array().map(|cs| cs.iter().map(|c| {
            let extra: Map<String, Value> = c.as_object().map(|o| o.iter()
                .filter(|(k, _)| !["id", "type", "function", "index"].contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default();
            ToolCall {
                id: c["id"].as_str().filter(|s| !s.is_empty()).map(String::from),
                name: c["function"]["name"].as_str().unwrap_or("").into(),
                arguments: c["function"]["arguments"].clone(),
                extra: (!extra.is_empty()).then_some(extra),
            }
        }).collect()).unwrap_or_default();
        Ok(Completion {
            content: msg["content"].as_str().unwrap_or("").into(),
            tool_calls: calls,
            reasoning: msg["reasoning_content"].as_str().filter(|s| !s.is_empty()).map(String::from),
            raw: None,
        })
    }

    pub fn list_models(&self) -> Result<Vec<String>, LlmError> {
        Ok(self.list_models_labelled()?.into_iter().map(|(name, _)| name).collect())
    }

    /// Chat models as (name, label). For Ollama, embedding-only models are left out (they can't chat) and
    /// the label shows the size and whether the model can call tools, from /api/tags.
    pub fn list_models_labelled(&self) -> Result<Vec<(String, String)>, LlmError> {
        if self.is_ollama() {
            let data = self.send(self.client.get(format!("{}/api/tags", self.url)), 8)?;
            return Ok(ollama_chat_models(&data));
        }
        if self.is_anthropic() {
            let data = self.send(self.anthropic_get("/models?limit=1000")?, 15)?;
            let mut models: Vec<(String, String)> = data["data"].as_array().into_iter().flatten().filter_map(|m| {
                let id = m["id"].as_str()?.to_string();
                let label = m["display_name"].as_str().map(|d| format!("{d} · {id}")).unwrap_or_else(|| id.clone());
                Some((id, label))
            }).collect();
            models.sort();
            return Ok(models);
        }
        let data = self.send(self.client.get(format!("{}/models", self.url)).header("Authorization", self.auth()?), 15)?;
        let mut models: Vec<String> = data["data"].as_array().map(|ms| ms.iter()
            .filter_map(|m| m["id"].as_str().map(|s| s.trim_start_matches("models/").to_string()))
            .filter(|id| self.provider != "openai" || openai_chat_model(id)).collect())
            .unwrap_or_default();
        models.sort();
        Ok(models.into_iter().map(|m| (m.clone(), m)).collect())
    }

    pub fn health(&self) -> Value {
        let info = json!({"provider": self.provider, "label": self.label, "model": self.model, "local": self.local});
        match self.list_models_labelled() {
            Ok(models) => {
                let installed = models.iter().any(|(n, _)| *n == self.model);
                let mut v = info;
                v["ok"] = json!(true);
                v["model_installed"] = json!(installed);
                let models: Vec<(String, String)> = models.into_iter().take(200).collect();
                v["model_labels"] = json!(models.iter().map(|(n, l)| (n.clone(), json!(l))).collect::<Map<String, Value>>());
                v["models"] = json!(models.into_iter().map(|(n, _)| n).collect::<Vec<_>>());
                v
            }
            Err(e) => {
                let mut v = info;
                v["ok"] = json!(false);
                v["error"] = json!(e.0);
                v
            }
        }
    }
}
