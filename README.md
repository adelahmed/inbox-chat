# Inbox Chat

A Windows desktop app that puts an AI assistant next to your **classic Outlook** mailbox. It shows your
folders, messages and a reading pane like Outlook does, and adds a chat where you can ask about your mail
and have replies, forwards and new emails prepared for you.

**It never sends email.** Everything it writes is saved in Outlook's **Drafts** folder; you review and
send from Outlook yourself.

You choose the model: a local [Ollama](https://ollama.com) model (mail stays on your network) or a cloud
LLM: OpenAI ChatGPT, Anthropic Claude, Google Gemini or Moonshot Kimi.

## Download

**[⬇ InboxChat.exe — latest release](../../releases/latest/download/InboxChat.exe)** ·
[all releases](../../releases)

- Windows 10 or 11 with the WebView2 runtime (built into Windows 11 and current Windows 10).
- **Classic** Outlook (the desktop app). The *new* Outlook can't be automated, so it isn't supported.
- No installer and no admin rights: the `.exe` runs as is. It isn't code-signed, so Windows SmartScreen
  may warn the first time: *More info → Run anyway*.

Try it without touching your mailbox: `InboxChat.exe --demo` opens a sample mailbox and saves nothing.

## First start

On a new installation a welcome window offers two ways in:

1. **Import a settings file** — a JSON file exported from another PC or handed to you by a colleague
   (see [Settings files](#settings-files)).
2. **Set up manually** — open **Settings**, pick a provider and model (and paste an API key for a cloud
   provider), then open **Profile** and describe yourself: how you appear in To/CC, your team, its
   keywords, the language you write in and, if you like, your greeting and sign-off.

Nothing about any particular person or organisation is built in; every user starts from neutral defaults.

**Help** (top right, or <kbd>F1</kbd>) is built into the app, in **English and Arabic**: an introduction, a
user guide, a configuration guide, data & privacy (including where the data file is) and troubleshooting.
The pages are in [`ui/help/`](ui/help/) (Arabic: [`ui/help/ar/`](ui/help/ar/)) and can also be read in a
browser.

## What it does

| Pane                   |                                                                                                                                                                                                                                                |
| ---------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Folders**      | Your folder tree with unread counts.                                                                                                                                                                                                           |
| **Messages**     | Newest first, 50 at a time. Badges show mail **to you**, **CC**, **mentions you**, for your **department** and **requests**; *For me* hides the rest.                                                          |
| **Reading pane** | The email as Outlook shows it — HTML, tables and formatting, inline pictures, meeting invitations (their rich-text body is recovered from Outlook's RTF) — then attachments and the conversation history.                                    |
| **Assistant**    | Chat about the open email or the whole mailbox: "summarize this thread", "reply that Wednesday works", "forward to Sara", "what needs my action this week?", "what do I need to follow up on?". Answers cite emails as `m12`; click to open. |

- **✨ panel.** For each email you open: a short summary, what you need to do, and a suggested reply in
  your style, editable and saved to Drafts with one click.
- **Your writing style, applied by code.** Greeting (one recipient / several / special people), sign-off,
  font and direction are added to every draft whatever the model writes. Arabic greetings join
  correctly (ل + ال → لل).
- **Learning.** The app reads Sent Items in the background and keeps, on this PC only, your past replies
  (as examples of tone and length) and who you write to (so "Sara" or «سارة» resolves to the right
  address; when several people match, it asks instead of guessing).
- **Follow-ups.** Requests you sent in the last 10 days that nobody on the To line has answered yet.
- **Safe reading pane.** Email HTML is sandboxed (scripts never run), external pictures aren't
  downloaded, links ask before opening, risky attachment types are refused, and opened attachments are
  marked as downloaded from the internet so Office uses Protected View.

## LLM providers

| Provider                   | Default URL                                                 | Default model            | Key                   |
| -------------------------- | ----------------------------------------------------------- | ------------------------ | --------------------- |
| Ollama (local or intranet) | `http://localhost:11434`                                  | `qwen3:8b`             | —                    |
| OpenAI ChatGPT             | `https://api.openai.com/v1`                               | `gpt-5.1`              | `OPENAI_API_KEY`    |
| Anthropic Claude           | `https://api.anthropic.com/v1`                            | `claude-opus-5-5`      | `ANTHROPIC_API_KEY` |
| Google Gemini              | `https://generativelanguage.googleapis.com/v1beta/openai` | `gemini-2.5-flash`     | `GEMINI_API_KEY`    |
| Moonshot Kimi              | `https://api.moonshot.ai/v1`                              | `kimi-k2-0905-preview` | `MOONSHOT_API_KEY`  |

The **Model** box lists the models the server offers (for Ollama: chat models only, with their size and
whether they can call tools — the assistant needs tools). API keys are encrypted with Windows DPAPI for
your Windows account and never shown to the window.

> **Privacy.** With a cloud provider, the emails the assistant reads to answer you — and, if automatic
> suggestions are on, each email you open — are sent to that provider. Settings shows a warning and the
> header a **cloud** badge. Check your organisation's data policy before using a cloud provider with
> work mail; use Ollama to keep mail on your network.

## Settings files

**Settings → Import / export** saves the LLM settings and your Profile (department, keywords, language,
writing style, learning options) as one JSON file, and loads such a file on another PC or for another
user. API keys are left out unless you tick *Include API keys* (they are then in plain text — keep that
file private). Learned replies, conversations and caches are never exported.

```json
{
  "format": "inbox-chat-settings",
  "version": 1,
  "llm": { "provider": "ollama", "temperature": 0.2, "num_ctx": 16384,
           "providers": { "ollama": { "url": "http://gpu-server:11434", "model": "qwen3:14b", "use_proxy": false } } },
  "profile": { "dept_name": "Customer Experience", "language": "English",
               "style": { "enabled": true, "greeting_single": "Dear {name},", "greeting_multi": "Dear all,",
                          "signoff": "Best regards\nSam", "font_family": "Calibri", "font_size_pt": 11, "rtl": false } }
}
```

Either part may be left out; missing fields keep their defaults. A file is checked completely before
anything is saved, so a bad file changes nothing. This is a handy way for a team to share one starting
setup (server address, model, department background) — each person then adds their own name and style.

## Where data is kept

One SQLite file per Windows user: `%APPDATA%\InboxChat\inbox-chat.db`, usually
`C:\Users\<you>\AppData\Roaming\InboxChat\inbox-chat.db` (set `INBOX_CHAT_DB` to use another). It holds
the settings, encrypted API keys, conversations, the short email ids (`m12`), a log of drafts the
assistant saved, your profile, learned replies, correspondents, follow-ups and cached ✨ panels.
**Don't delete it**: the app would start over as if newly installed. To back it up, close the app and
copy the file. Help → Data & privacy shows the exact path on your PC.

Nothing is written to Outlook except drafts and, if enabled in Profile, marking an email read when you
open it.

Environment variables can preset defaults (saved settings take precedence): `INBOX_CHAT_PROVIDER`,
`OLLAMA_URL`, `OLLAMA_MODEL`, `OLLAMA_NUM_CTX`, `OLLAMA_THINK`, `OLLAMA_USE_PROXY`, the API key
variables above, `INBOX_CHAT_USER`, `INBOX_CHAT_CA_BUNDLE` (a PEM root CA for TLS-inspecting proxies)
and `INBOX_CHAT_DEMO=1` (same as `--demo`).

## Build from source

Requirements: Rust (stable, MSVC toolchain) and the Visual Studio C++ build tools.

```
cargo test --lib                          # unit tests: sample mailbox + fake LLM servers, no Outlook needed
cargo run -- --demo                       # the app on the sample mailbox
cargo build --release                     # target\release\inbox-chat.exe
cargo run --bin outlook-selftest          # read-only checks against your real Outlook (prints counts only)
cargo run --bin outlook-selftest -- --bodies   # what the reading pane gets for recent Inbox items (counts only)
```

**Releases.** Pushing a tag that matches the version in `Cargo.toml` (e.g. `v0.2.0`) runs
[`.github/workflows/release.yml`](.github/workflows/release.yml): tests, a release build, and a GitHub
Release with `InboxChat.exe` and its SHA-256 checksum attached. The download link above always points
to the newest one.

| Path                                                                          |                                                                                                                                                    |
| ----------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| `src/outlook/`                                                              | Classic Outlook over COM on one dedicated thread. `dispatch.rs` is a small late-binding IDispatch helper that refuses any method named `Send`. |
| `src/mail.rs`, `src/fake_mail.rs`                                         | The mailbox interface, and the sample mailbox used by tests and `--demo`                                                                         |
| `src/rtf.rs`                                                                | Rich-text bodies (meeting invitations): recovers HTML wrapped in RTF, converts plain RTF                                                           |
| `src/tools.rs`                                                              | The tools the LLM can call, and short email ids                                                                                                    |
| `src/llm.rs`, `src/agent.rs`                                              | Provider backends (Ollama, Anthropic, OpenAI-compatible) and the tool-calling loop                                                                 |
| `src/profile.rs`, `src/context.rs`, `src/learn.rs`, `src/followup.rs` | Profile and writing style, relevance badges, Arabic/Latin name matching, learning, follow-ups                                                      |
| `src/settings.rs`, `src/transfer.rs`, `src/store.rs`                    | Settings with DPAPI-encrypted keys, settings files, the SQLite store                                                                               |
| `src/lib.rs`                                                                | The commands the window calls                                                                                                                      |
| `ui/`                                                                       | The window: `index.html`, `app.css`, `app.js` (no frameworks)                                                                                |
| `ui/help/`                                                                  | The Help pages, English and Arabic (`ar/`): plain HTML + `help.css`, no scripts, embedded in the exe                                           |

## Troubleshooting

- **Outlook isn't open:** the app starts classic Outlook minimized and connects once it has loaded. If
  it still can't connect, finish any Outlook prompt (profile, password) and click ⟳ in Folders.
- **"A program is trying to access e-mail…"**: Outlook's object-model guard, set by IT policy.
- **LLM status is red:** start Ollama or fix its URL; for cloud providers check the key and, on a
  corporate network, tick *Use proxy* (a PAC-only proxy needs its address typed in). For TLS errors
  behind an inspecting proxy, set `INBOX_CHAT_CA_BUNDLE`.

## Author

|                              |                                                        |
| ---------------------------- | ------------------------------------------------------ |
| **Author / Developer** | Adel F. Ahmed                                          |
| **Email**              | [adel.f.ahmed@gmail.com](mailto:adel.f.ahmed@gmail.com) |
| **Website**            | [www.adelahmed.com](https://www.adelahmed.com)          |
| **Issues**             | [Report a problem or suggest a feature](../../issues)   |

## License

[MIT](LICENSE) © 2026 Adel F. Ahmed. Not affiliated with Microsoft, OpenAI, Anthropic, Google or Moonshot.
