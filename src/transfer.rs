//! Settings files: the LLM settings and the profile (department, writing style, learning options) as
//! one JSON file, to move them to another PC or hand a starting setup to a colleague. API keys are left
//! out unless the user asks for them — they are then in plain text in the file.

use crate::context::UserContext;
use crate::profile::Profile;
use crate::settings::{provider_info, Settings};
use serde_json::{json, Value};

pub const FORMAT: &str = "inbox-chat-settings";
pub const VERSION: u64 = 1;
/// Larger files are refused before reading.
pub const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;

/// The settings file's content.
pub fn export(settings: &Settings, ctx: &UserContext, include_keys: bool) -> Value {
    let llm = settings.export(include_keys);
    let keys = llm["providers"].as_object().map(|p| p.values().filter(|v| v.get("api_key").is_some()).count()).unwrap_or(0);
    json!({
        "format": FORMAT,
        "version": VERSION,
        "app_version": env!("CARGO_PKG_VERSION"),
        "exported_at": chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string(),
        "includes_api_keys": keys > 0,
        "llm": llm,
        "profile": ctx.profile(),
    })
}

/// Apply a settings file. Everything is checked before anything is saved, so a bad file changes
/// nothing. Either part may be missing; API keys missing from the file keep the ones already saved.
/// Returns what was imported, for the UI.
pub fn import(settings: &Settings, ctx: &UserContext, data: &Value) -> Result<Value, String> {
    let obj = data.as_object().ok_or("This is not an Inbox Chat settings file.")?;
    match obj.get("format").and_then(Value::as_str) {
        Some(FORMAT) => {}
        Some(other) => return Err(format!("This is not an Inbox Chat settings file (format '{other}').")),
        None if obj.contains_key("llm") || obj.contains_key("profile") => {}
        None => return Err("This is not an Inbox Chat settings file.".into()),
    }
    if obj.get("version").and_then(Value::as_u64).is_some_and(|v| v > VERSION) {
        return Err("This file was made by a newer version of Inbox Chat. Update the app, then import it.".into());
    }
    let (llm, profile) = (obj.get("llm").filter(|v| !v.is_null()), obj.get("profile").filter(|v| !v.is_null()));
    if llm.is_none() && profile.is_none() {
        return Err("The file has no settings in it.".into());
    }

    let mut notes = Vec::new();
    let llm_patch = match llm {
        Some(v) => {
            let mut v = v.clone();
            let o = v.as_object_mut().ok_or("LLM settings: expected an object.")?;
            // Providers this version doesn't know (a file from a newer version) are skipped.
            if let Some(provs) = o.get_mut("providers").and_then(Value::as_object_mut) {
                let unknown: Vec<String> = provs.keys().filter(|k| provider_info(k).is_none()).cloned().collect();
                for k in unknown {
                    provs.remove(&k);
                    notes.push(format!("Skipped the unknown provider '{k}'."));
                }
            }
            if o.get("provider").and_then(Value::as_str).is_some_and(|p| provider_info(p).is_none()) {
                o.remove("provider");
                notes.push("The file's chosen provider is unknown here; the current one was kept.".into());
            }
            o.remove("config_file");
            settings.preview(&v).map_err(|e| format!("LLM settings: {e}"))?;
            Some(v)
        }
        None => None,
    };
    let profile = match profile {
        Some(p) => {
            let p: Profile = serde_json::from_value(p.clone()).map_err(|e| format!("Profile: {e}"))?;
            Some(UserContext::normalize(p).map_err(|e| format!("Profile: {e}"))?)
        }
        None => None,
    };

    let keys = llm_patch.as_ref().and_then(|v| v["providers"].as_object())
        .map(|p| p.values().filter(|x| x["api_key"].as_str().is_some_and(|k| !k.trim().is_empty())).count()).unwrap_or(0);
    if let Some(v) = &llm_patch {
        settings.update(v)?;
    }
    let had_profile = profile.is_some();
    if let Some(p) = profile {
        ctx.save(p)?;
    }
    Ok(json!({"llm": llm_patch.is_some(), "profile": had_profile, "api_keys": keys, "notes": notes}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_mail::{sample_profile, FakeMail};
    use crate::store::Store;
    use std::sync::Arc;

    fn install() -> (Arc<Settings>, UserContext) {
        let store = Arc::new(Store::in_memory());
        let settings = Arc::new(Settings::load(store.clone()));
        (settings, UserContext::load(store, Arc::new(FakeMail::seeded())))
    }

    #[test]
    fn round_trip_to_a_new_installation() {
        let (settings, ctx) = install();
        settings.update(&json!({"provider": "anthropic", "temperature": 0.4,
            "providers": {"anthropic": {"api_key": "sk-ant-secret-1"}, "ollama": {"url": "http://gpu-box:11434", "model": "qwen3:14b"}}})).unwrap();
        ctx.save(sample_profile()).unwrap();

        let file = export(&settings, &ctx, false);
        assert_eq!((file["format"].as_str(), file["version"].as_u64()), (Some(FORMAT), Some(VERSION)));
        assert_eq!(file["includes_api_keys"], false);
        assert!(!file.to_string().contains("sk-ant-secret"), "no keys unless asked for");
        assert_eq!(file["profile"]["style"]["signoff"], "مع تحياتي\n-عادل");

        let (s2, c2) = install();
        assert!(s2.never_saved() && c2.never_saved());
        let r = import(&s2, &c2, &file).unwrap();
        assert_eq!(r, json!({"llm": true, "profile": true, "api_keys": 0, "notes": []}));
        assert_eq!(s2.get().provider, "anthropic");
        assert_eq!(s2.get().providers["ollama"].url, "http://gpu-box:11434");
        assert_eq!(s2.get().providers["anthropic"].api_key, "", "the key has to be entered again");
        assert_eq!(c2.profile(), ctx.profile());
        assert!(!s2.never_saved() && !c2.never_saved());

        let with_keys = export(&settings, &ctx, true);
        assert_eq!(with_keys["includes_api_keys"], true);
        let (s3, c3) = install();
        assert_eq!(import(&s3, &c3, &with_keys).unwrap()["api_keys"], 1);
        assert_eq!(s3.get().providers["anthropic"].api_key, "sk-ant-secret-1");
    }

    #[test]
    fn partial_newer_and_bad_files() {
        let (settings, ctx) = install();
        // Profile only: the LLM settings stay as they are.
        let r = import(&settings, &ctx, &json!({"format": FORMAT, "version": 1, "profile": {"language": "Arabic"}})).unwrap();
        assert_eq!((r["llm"].as_bool(), r["profile"].as_bool()), (Some(false), Some(true)));
        assert_eq!(ctx.profile().language, "Arabic");
        assert!(settings.never_saved());
        // A file from a newer version: unknown providers skipped, the rest applied.
        let r = import(&settings, &ctx, &json!({"format": FORMAT, "llm": {"provider": "mistral",
            "providers": {"mistral": {"url": "https://x"}, "gemini": {"model": "gemini-x"}}}})).unwrap();
        assert_eq!(r["notes"].as_array().unwrap().len(), 2, "{r}");
        assert_eq!(settings.get().providers["gemini"].model, "gemini-x");
        assert_eq!(settings.get().provider, "ollama");

        let before = (settings.get(), ctx.profile());
        let bad = [
            json!([1, 2]),
            json!({"format": "something-else", "llm": {}}),
            json!({"format": FORMAT, "version": 99, "llm": {}}),
            json!({"format": FORMAT}),
            json!({"hello": "world"}),
            // Invalid parts: nothing at all is saved, not even the valid part.
            json!({"format": FORMAT, "llm": {"provider": "gemini"}, "profile": {"style": {"greeting_single": "Hello"}}}),
            json!({"format": FORMAT, "llm": {"providers": {"ollama": {"url": "ftp://x"}}}, "profile": {"language": "English"}}),
            json!({"format": FORMAT, "profile": {"my_names": "not a list"}}),
        ];
        for b in &bad {
            assert!(import(&settings, &ctx, b).is_err(), "{b}");
        }
        assert_eq!((settings.get(), ctx.profile()), before, "a rejected file changes nothing");
        assert!(import(&settings, &ctx, &bad[2]).unwrap_err().contains("newer version"));
    }
}
