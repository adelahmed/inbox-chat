//! LLM settings edited from the Settings panel and saved in the SQLite store.
//! API keys are encrypted with Windows DPAPI (tied to the current Windows user) and never leave the
//! Rust side: `public()` replaces them with a hint.
//!
//! Precedence: built-in defaults < environment variables < saved settings.

use crate::store::Store;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

pub struct ProviderInfo {
    pub name: &'static str,
    pub label: &'static str,
    pub local: bool,
    pub url: &'static str,
    pub model: &'static str,
    pub use_proxy: bool,
    pub key_env: &'static [&'static str],
}

/// In the order the Settings panel shows them.
pub const PROVIDERS: [ProviderInfo; 5] = [
    ProviderInfo { name: "ollama", label: "Ollama", local: true, url: "http://localhost:11434",
                   model: "qwen3:8b", use_proxy: false, key_env: &[] },
    ProviderInfo { name: "openai", label: "OpenAI ChatGPT", local: false, url: "https://api.openai.com/v1",
                   model: "gpt-5.1", use_proxy: true, key_env: &["OPENAI_API_KEY"] },
    ProviderInfo { name: "anthropic", label: "Anthropic Claude", local: false, url: "https://api.anthropic.com/v1",
                   model: "claude-opus-5-5", use_proxy: true, key_env: &["ANTHROPIC_API_KEY"] },
    ProviderInfo { name: "gemini", label: "Google Gemini", local: false,
                   url: "https://generativelanguage.googleapis.com/v1beta/openai", model: "gemini-2.5-flash",
                   use_proxy: true, key_env: &["GEMINI_API_KEY", "GOOGLE_API_KEY"] },
    ProviderInfo { name: "kimi", label: "Moonshot Kimi", local: false, url: "https://api.moonshot.ai/v1",
                   model: "kimi-k2-0905-preview", use_proxy: true, key_env: &["MOONSHOT_API_KEY", "KIMI_API_KEY"] },
];

pub fn provider_info(name: &str) -> Option<&'static ProviderInfo> {
    PROVIDERS.iter().find(|p| p.name == name)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderCfg {
    pub url: String,
    pub model: String,
    pub use_proxy: bool,
    #[serde(default)]
    pub proxy_url: String,
    #[serde(skip)]
    pub api_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    pub provider: String,
    pub temperature: f64,
    pub num_ctx: u32,
    #[serde(default)]
    pub think: String,
    #[serde(default)]
    pub user_name: String,
    pub providers: BTreeMap<String, ProviderCfg>,
}

impl Config {
    pub fn active(&self) -> &ProviderCfg {
        &self.providers[&self.provider]
    }
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn env_defaults() -> Config {
    defaults_from(env)
}

/// Built-in defaults, overridden by the given environment lookup.
fn defaults_from(env: impl Fn(&str) -> Option<String>) -> Config {
    let mut providers = BTreeMap::new();
    for p in &PROVIDERS {
        providers.insert(p.name.to_string(), ProviderCfg {
            url: p.url.into(),
            model: p.model.into(),
            use_proxy: p.use_proxy,
            proxy_url: String::new(),
            api_key: p.key_env.iter().find_map(|n| env(n)).unwrap_or_default(),
        });
    }
    let o = providers.get_mut("ollama").unwrap();
    if let Some(u) = env("OLLAMA_URL") { o.url = u; }
    if let Some(m) = env("OLLAMA_MODEL") { o.model = m; }
    o.use_proxy = env("OLLAMA_USE_PROXY").as_deref() == Some("1");
    let provider = env("INBOX_CHAT_PROVIDER").filter(|p| provider_info(p).is_some()).unwrap_or_else(|| "ollama".into());
    Config {
        provider,
        temperature: 0.2,
        num_ctx: env("OLLAMA_NUM_CTX").and_then(|v| v.parse().ok()).unwrap_or(16384),
        think: env("OLLAMA_THINK").unwrap_or_default(),
        user_name: env("INBOX_CHAT_USER").unwrap_or_default(),
        providers,
    }
}

/// Merge a (partial) JSON patch into `cfg`, validating every field.
pub fn apply_patch(cfg: &mut Config, patch: &Value) -> Result<(), String> {
    let Some(obj) = patch.as_object() else { return Err("settings must be a JSON object".into()) };
    if let Some(p) = obj.get("provider") {
        let p = p.as_str().unwrap_or_default();
        if provider_info(p).is_none() {
            let names: Vec<&str> = PROVIDERS.iter().map(|p| p.name).collect();
            return Err(format!("Unknown provider '{p}'. Choose: {}", names.join(", ")));
        }
        cfg.provider = p.into();
    }
    if let Some(t) = obj.get("temperature") {
        let t = t.as_f64().ok_or("temperature must be a number")?;
        cfg.temperature = t.clamp(0.0, 2.0);
    }
    if let Some(n) = obj.get("num_ctx") {
        let n = n.as_f64().ok_or("num_ctx must be a number")?;
        cfg.num_ctx = (n as i64).clamp(2048, 262_144) as u32;
    }
    if let Some(t) = obj.get("think") {
        let t = t.as_str().unwrap_or_default().to_lowercase();
        if !["", "true", "false"].contains(&t.as_str()) {
            return Err("think must be '', 'true' or 'false'".into());
        }
        cfg.think = t;
    }
    if let Some(u) = obj.get("user_name") {
        cfg.user_name = u.as_str().unwrap_or_default().trim().chars().take(100).collect();
    }
    if let Some(provs) = obj.get("providers").and_then(Value::as_object) {
        for (name, p) in provs {
            let info = provider_info(name).ok_or_else(|| format!("Unknown provider '{name}'"))?;
            let cur = cfg.providers.get_mut(name).expect("all providers present");
            if let Some(u) = p.get("url") {
                let u = u.as_str().unwrap_or_default().trim().trim_end_matches('/');
                let u = if u.is_empty() { info.url } else { u };
                if !(u.starts_with("http://") || u.starts_with("https://")) {
                    return Err(format!("{name}: URL must start with http:// or https://"));
                }
                cur.url = u.into();
            }
            if let Some(m) = p.get("model") {
                let m = m.as_str().unwrap_or_default().trim();
                cur.model = if m.is_empty() { info.model.into() } else { m.into() };
            }
            if let Some(b) = p.get("use_proxy") {
                cur.use_proxy = b.as_bool().unwrap_or(false);
            }
            if let Some(x) = p.get("proxy_url") {
                cur.proxy_url = x.as_str().unwrap_or_default().trim().into();
            }
            // An empty api_key means "keep the current one"; clearing is explicit.
            if p.get("clear_api_key").and_then(Value::as_bool).unwrap_or(false) {
                cur.api_key.clear();
            } else if let Some(k) = p.get("api_key").and_then(Value::as_str).map(str::trim).filter(|k| !k.is_empty()) {
                cur.api_key = k.into();
            }
        }
    }
    Ok(())
}

pub struct Settings {
    store: Arc<Store>,
    data: Mutex<Config>,
    pub load_error: Option<String>,
}

const KV_KEY: &str = "llm_settings";

impl Settings {
    pub fn load(store: Arc<Store>) -> Self {
        Self::load_over(store, env_defaults())
    }

    /// Saved settings on top of `cfg` (tests use defaults without the environment).
    fn load_over(store: Arc<Store>, mut cfg: Config) -> Self {
        let mut load_error = None;
        match store.kv_get(KV_KEY) {
            Ok(Some(saved)) => match serde_json::from_str::<Value>(&saved) {
                Ok(v) => {
                    if let Err(e) = apply_patch(&mut cfg, &v) {
                        load_error = Some(format!("Saved settings ignored in part: {e}"));
                    }
                }
                Err(e) => load_error = Some(format!("Saved settings unreadable: {e}")),
            },
            Ok(None) => {}
            Err(e) => load_error = Some(format!("Could not read settings: {e}")),
        }
        for (name, p) in cfg.providers.iter_mut() {
            match store.secret_get(name) {
                Ok(Some(blob)) => match dpapi::unprotect(&blob) {
                    Ok(key) => p.api_key = key,
                    Err(e) => load_error = Some(format!("Could not decrypt the {name} API key: {e}")),
                },
                Ok(None) => {}
                Err(e) => load_error = Some(format!("Could not read the {name} API key: {e}")),
            }
        }
        Settings { store, data: Mutex::new(cfg), load_error }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Config> {
        self.data.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn get(&self) -> Config {
        self.lock().clone()
    }

    /// Settings with `patch` applied, without saving (used by "Test connection").
    pub fn preview(&self, patch: &Value) -> Result<Config, String> {
        let mut cfg = self.get();
        apply_patch(&mut cfg, patch)?;
        Ok(cfg)
    }

    pub fn update(&self, patch: &Value) -> Result<Value, String> {
        let mut guard = self.lock();
        let mut cfg = guard.clone();
        apply_patch(&mut cfg, patch)?;
        let plain = serde_json::to_string(&cfg).map_err(|e| e.to_string())?; // api_key is #[serde(skip)]
        for (name, p) in &cfg.providers {
            if p.api_key == guard.providers[name].api_key && self.store.secret_get(name).ok().flatten().is_some() {
                continue; // unchanged and already saved
            }
            if p.api_key.is_empty() {
                self.store.secret_delete(name).map_err(|e| format!("Could not save settings: {e}"))?;
            } else {
                let blob = dpapi::protect(&p.api_key).map_err(|e| format!("Could not encrypt the API key: {e}"))?;
                self.store.secret_set(name, &blob).map_err(|e| format!("Could not save settings: {e}"))?;
            }
        }
        self.store.kv_set(KV_KEY, &plain).map_err(|e| format!("Could not save settings: {e}"))?;
        *guard = cfg;
        drop(guard);
        Ok(self.public())
    }

    /// True until settings have been saved once on this PC (first start of a new installation).
    pub fn never_saved(&self) -> bool {
        self.store.kv_get(KV_KEY).ok().flatten().is_none()
    }

    /// The LLM settings for a settings file. API keys only when asked for: they are then in plain text.
    pub fn export(&self, include_keys: bool) -> Value {
        let cfg = self.get();
        let providers: serde_json::Map<String, Value> = PROVIDERS.iter().map(|info| {
            let p = &cfg.providers[info.name];
            let mut v = json!({"url": p.url, "model": p.model, "use_proxy": p.use_proxy, "proxy_url": p.proxy_url});
            if include_keys && !p.api_key.is_empty() {
                v["api_key"] = json!(p.api_key);
            }
            (info.name.to_string(), v)
        }).collect();
        json!({"provider": cfg.provider, "temperature": cfg.temperature, "num_ctx": cfg.num_ctx, "think": cfg.think,
               "user_name": cfg.user_name, "providers": providers})
    }
    /// Settings safe to send to the web view: API keys replaced by a hint.
    pub fn public(&self) -> Value {
        let cfg = self.get();
        let mut provs = serde_json::Map::new();
        for (name, p) in &cfg.providers {
            let info = provider_info(name).unwrap();
            let key = &p.api_key;
            let n = key.chars().count();
            let hint = if n >= 8 {
                format!("…{}", key.chars().skip(n - 4).collect::<String>())
            } else if n > 0 { "set".into() } else { String::new() };
            provs.insert(name.clone(), json!({
                "needs_key": !info.key_env.is_empty(),
                "url": p.url, "model": p.model, "use_proxy": p.use_proxy, "proxy_url": p.proxy_url,
                "has_api_key": !key.is_empty(), "api_key_hint": hint,
                "label": info.label, "local": info.local, "default_url": info.url, "default_model": info.model,
            }));
        }
        let mut out = json!({
            "provider": cfg.provider, "temperature": cfg.temperature, "num_ctx": cfg.num_ctx,
            "think": cfg.think, "user_name": cfg.user_name, "providers": provs,
            "order": PROVIDERS.iter().map(|p| p.name).collect::<Vec<_>>(),
            "config_file": self.store.path.display().to_string(),
        });
        if let Some(e) = &self.load_error {
            out["load_error"] = json!(e);
        }
        out
    }
}

/// Windows DPAPI: encrypts for the current Windows user.
pub mod dpapi {
    #[cfg(windows)]
    pub fn protect(secret: &str) -> Result<Vec<u8>, String> {
        use windows::core::w;
        use windows::Win32::Security::Cryptography::{CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB};
        let data = secret.as_bytes();
        let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
        let mut out = CRYPT_INTEGER_BLOB::default();
        unsafe {
            CryptProtectData(&input, w!("InboxChat"), None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out)
                .map_err(|e| e.message().to_string())?;
            Ok(take_blob(out))
        }
    }

    #[cfg(windows)]
    pub fn unprotect(blob: &[u8]) -> Result<String, String> {
        use windows::Win32::Security::Cryptography::{CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB};
        let input = CRYPT_INTEGER_BLOB { cbData: blob.len() as u32, pbData: blob.as_ptr() as *mut u8 };
        let mut out = CRYPT_INTEGER_BLOB::default();
        unsafe {
            CryptUnprotectData(&input, None, None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out)
                .map_err(|e| e.message().to_string())?;
            String::from_utf8(take_blob(out)).map_err(|e| e.to_string())
        }
    }

    #[cfg(windows)]
    unsafe fn take_blob(b: windows::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB) -> Vec<u8> {
        use windows::Win32::Foundation::{LocalFree, HLOCAL};
        let v = std::slice::from_raw_parts(b.pbData, b.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(b.pbData as _)));
        v
    }

    #[cfg(not(windows))]
    pub fn protect(secret: &str) -> Result<Vec<u8>, String> {
        Ok(secret.as_bytes().to_vec())
    }

    #[cfg(not(windows))]
    pub fn unprotect(blob: &[u8]) -> Result<String, String> {
        String::from_utf8(blob.to_vec()).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Settings with the built-in defaults only, whatever the environment of the test machine.
    fn fresh() -> (Arc<Store>, Settings) {
        let store = Arc::new(Store::in_memory());
        let s = Settings::load_over(store.clone(), defaults_from(|_| None));
        (store, s)
    }

    #[test]
    fn rejects_bad_values_and_keeps_state() {
        let (_, s) = fresh();
        assert!(s.update(&json!({"provider": "chatgpt"})).unwrap_err().contains("Unknown provider"));
        assert!(s.update(&json!({"providers": {"gemini": {"url": "ftp://x"}}})).is_err());
        assert_eq!(s.get().providers["gemini"].url, provider_info("gemini").unwrap().url);
    }

    #[test]
    fn keys_are_encrypted_masked_and_kept() {
        let (store, s) = fresh();
        let key = "test-key-123456";
        let public = s.update(&json!({"provider": "gemini", "providers": {"gemini": {"api_key": key, "url": "https://x/"}}})).unwrap();
        assert!(!public.to_string().contains(key), "key never in public settings");
        assert_eq!(public["providers"]["gemini"]["api_key_hint"], "…3456");
        assert_eq!(public["providers"]["gemini"]["url"], "https://x");
        let blob = store.secret_get("gemini").unwrap().unwrap();
        #[cfg(windows)]
        assert!(!String::from_utf8_lossy(&blob).contains(key), "DPAPI-encrypted at rest");
        assert!(!store.kv_get(KV_KEY).unwrap().unwrap().contains(key), "key not in plain settings");
        s.update(&json!({"providers": {"gemini": {"api_key": ""}}})).unwrap();
        assert_eq!(s.get().providers["gemini"].api_key, key, "empty field keeps key");
        let reloaded = Settings::load_over(store.clone(), defaults_from(|_| None));
        assert_eq!(reloaded.get().provider, "gemini");
        assert_eq!(reloaded.get().providers["gemini"].api_key, key, "key survives restart");
        s.update(&json!({"providers": {"gemini": {"clear_api_key": true}}})).unwrap();
        assert!(store.secret_get("gemini").unwrap().is_none(), "key removed");
    }

    #[test]
    fn generic_defaults_and_export() {
        let (_, s) = fresh();
        let cfg = s.get();
        assert_eq!(cfg.provider, "ollama");
        assert_eq!(cfg.providers["ollama"].url, "http://localhost:11434", "no organisation-specific server by default");
        assert_eq!(cfg.providers.len(), PROVIDERS.len());
        assert!(s.never_saved());
        let public = s.public();
        assert_eq!(public["order"], json!(["ollama", "openai", "anthropic", "gemini", "kimi"]));
        assert_eq!(public["providers"]["anthropic"]["needs_key"], true);
        assert_eq!(public["providers"]["ollama"]["needs_key"], false);

        s.update(&json!({"provider": "anthropic", "user_name": "Sam",
                         "providers": {"anthropic": {"api_key": "sk-ant-test-1234"}, "openai": {"model": "gpt-x"}}})).unwrap();
        assert!(!s.never_saved());
        let no_keys = s.export(false);
        assert_eq!(no_keys["provider"], "anthropic");
        assert_eq!(no_keys["providers"]["openai"]["model"], "gpt-x");
        assert!(!no_keys.to_string().contains("sk-ant-test"), "keys left out unless asked for");
        assert_eq!(s.export(true)["providers"]["anthropic"]["api_key"], "sk-ant-test-1234");
        assert!(s.export(true)["providers"]["gemini"].get("api_key").is_none(), "no empty keys");

        // An exported file applied to a fresh installation gives the same settings.
        let (_, other) = fresh();
        other.update(&s.export(true)).unwrap();
        assert_eq!(other.get(), s.get());
    }
    #[test]
    fn preview_does_not_save() {
        let (_, s) = fresh();
        let cfg = s.preview(&json!({"provider": "kimi"})).unwrap();
        assert_eq!(cfg.provider, "kimi");
        assert_eq!(s.get().provider, "ollama");
    }
}
