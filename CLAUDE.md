# CLAUDE.md — Inbox Chat (Rust + Tauri 2)

Windows desktop client for **classic Outlook** with an LLM assistant: folder tree + message list | reading
pane (✨ insights panel, body, attachments, thread history) | chat. Public, MIT-licensed; see README.md.
Machine- or person-specific notes, if any, live in `CLAUDE.local.md` (gitignored, loaded automatically).

## Hard rules

1. **Never send email.** No `Send` anywhere; `Disp::invoke` refuses any method named `send` — keep that
   guard. Writes = reply / forward / new **drafts** via `.Save()`, plus **mark as read** when an email is
   opened in the reading pane or the assistant reads/drafts for it (`Profile.mark_read`, default on).
   `read_full`/`get_message` themselves never change `UnRead`. Any other write capability (move, flag,
   categorize, delete) must be proposed to the maintainer first; never add delete.
   - **Forwarding a meeting invitation** (Class 26/53–57) creates a normal email "FW: subject" with the
     invitation attached as an Outlook item (`Attachments.Add(item, olEmbeddeditem=5)`) and a details block
     (`text::invitation_details`, Arabic/RTL or English); never `MeetingItem.Forward()`. See `build_forward`.
   - Other drafts are written by `write_message`: HTMLBody for mail; items without HTMLBody via the hidden
     Word editor (`GetInspector.WordEditor`), closed after Save; plain Body as a last resort.
2. **Outlook not running:** `Conn::namespace` checks for OUTLOOK.EXE (Toolhelp) and, if absent, starts
   classic Outlook via ShellExecute `outlook.exe /recycle` (minimized, no focus), then polls `connect()` for
   up to 90 s; at most one relaunch every 2 min. Never rely on CoCreateInstance to start it (a hidden
   automation instance that dies with the app).
3. **All COM goes through `OutlookMail::run`** (one STA thread, `CoInitializeEx`). Closures get `&Disp`
   (the Namespace) and return plain data — never let a `Disp` escape. **Writes use `run(false, …)`** so a
   reconnect can't duplicate a draft.
4. **Mail content goes only to the LLM provider chosen in Settings.** API keys stay in Rust:
   `Settings::public()` masks them; stored DPAPI-encrypted in SQLite. Settings files include keys only when
   the user ticks *Include API keys* (plain text; the UI warns).
5. **Reading pane safety:** email HTML renders in `<iframe sandbox="allow-same-origin">` (never add
   `allow-scripts`) with an inner CSP `img-src data:` (no remote pictures). Attachments opened with
   ShellExecute get a Zone.Identifier (MOTW, ZoneId=3); `BLOCKED_EXT` (Outlook's Level-1 list) is never
   launched. Keep tables at their own widths: no `overflow-wrap:anywhere` or `table{max-width}` in the
   frame CSS (they squeeze columns to a letter each); wide content scrolls sideways.
6. **Tests:** `cargo test --lib` must pass; extend `fake_mail.rs` + tests for every change. Real-Outlook
   checks go in `src/bin/outlook-selftest.rs` and must stay **read-only and print no mail content**
   (counts, lengths, yes/no only). UI checks: `--demo` only, so no real mail appears in screenshots.
7. **Nobody's personal or organisation data in code, defaults, tests or fixtures.** Defaults are neutral
   (writing style off, empty profile, Ollama on localhost). Tests and `--demo` use the fictional
   `fake_mail::sample_profile()` and sample mailbox. Personal setups belong in a user's settings file.
8. **The user's writing style is applied by code** (`profile::DraftText::render`, `text::insert_styled`),
   never left to the model. Greeting from the draft's real To recipients: 1 → `greeting_single` with
   `{name}` (Arabic «ل{name}» joins: ل+ال → لل), ≥2 → `greeting_multi`, a sole recipient matching a
   `special` entry → its greeting; then the sign-off; font, size and direction. Model-written greetings,
   salaam openers and sign-offs are stripped (`strip_own_greeting_signoff`).
9. **Recipients: never guess.** `UserContext::resolve_recipients`: address → as is; exactly one
   correspondent match (name, address local part, or Arabic↔Latin `translit_matches`) → their address;
   several → keep the name and return `candidates`; none → Outlook `Resolve()`. "Hassan ≠ حسين" is a test.
10. **Learning** is read-only on Outlook and local-only in SQLite (`reply_examples`, `contacts`). Examples
    go to the LLM only inside a prompt the user triggered (chat) or the ✨ panel (auto unless turned off).
11. **The open email is the chat's default context.** `ChatAgent::open_email` puts its header and body
    (quoted history stripped, ≤3000 chars) in the system prompt ("OPEN EMAIL — the default subject of
    every instruction"). User messages store `viewing`/`viewing_subject`; the wire prefixes them
    `[About open email [mN] «subject»]`. `MailTools::call_with(.., viewing)` → `target()` uses the open
    email when the model gives no id or "this/current"; an explicit id is never overridden.
12. **What the user did with an email:** `MailStore::related` = PR_LAST_VERB_EXECUTED/TIME (replied /
    replied to all / forwarded) + Sent Items & Drafts items with the same `thread-topic` (fallback: subject
    LIKE) dated after it. Exposed as the `find_related` tool and `last_action` in `read_full`.
13. **Follow-ups** (`followup.rs`): Sent Items requests (`is_request`: whole-word ask verbs with و/ف
    prefixes, request phrases, '?' outside links — no bare nouns, no substrings) still waiting for a reply
    from a To recipient; shown for the last 10 days, still-waiting only, no status labels. Closed and
    dismissed rows are never re-checked.
14. **Settings files** (`transfer.rs`): `{"format": "inbox-chat-settings", "version": 1, "llm", "profile"}`.
    Import validates everything before saving anything; unknown providers are skipped with a note; a newer
    `version` is refused; missing keys keep the saved ones. New installations get a welcome dialog
    (`app_info.first_run`) offering import.
15. **Help stays current, in English and Arabic.** Every feature or behaviour change updates the matching
    page in `ui/help/` (English) **and** `ui/help/ar/` (Arabic, RTL) in the same change — introduction, user
    guide, configuration, data & privacy, troubleshooting, about: new options, buttons, defaults, data
    stored, what leaves the PC. Pages are plain HTML + the shared `help.css` (logical properties, so it works
    RTL), no scripts (CSP), same nav block on every page with a link to the other language's same page; web
    links open via `open_url` (handled in `app.js`). The UI is English, so Arabic pages name buttons in
    English inside `<span class="ui">` (an LTR island); Latin runs next to punctuation go in `<span class="en">`.
    Arabic wording: active voice (no المبني للمجهول; تم/يتم + مصدر is fine). Help opens in the language last
    read (`localStorage` `helpLang`), else Arabic when the Profile language is Arabic. Pictures in
    `ui/help/img/` come from `--demo` only. The Help footer shows `app_info.data_file`.

## Layout

```
src/lib.rs              Tauri commands (async → spawn_blocking), AppState, --demo, settings import/export dialogs
src/outlook/mod.rs      OutlookMail: COM thread + MailStore impl (folder tree, paging, read_full: HTMLBody or
                        RTFBody→HTML, inline cid images, attachments, forwards with Recipients.Resolve)
src/outlook/dispatch.rs Late-bound IDispatch (GetIDsOfNames/Invoke), OwnedVariant (VariantClear on drop), byte arrays
src/rtf.rs              RTFBody → HTML: \fromhtml de-encapsulation ([MS-OXRTFEX] 2.2.3.2), plain RTF conversion
src/mail.rs             MailStore trait + data types; dasl_filter, split_recipients, safe_file_name
src/fake_mail.rs        Sample mailbox + sample_profile() (tests + --demo); address book with an ambiguous "Rania"
src/tools.rs            Tool specs (incl. find_recipient, find_related, find_followups) + dispatcher; short ids
src/profile.rs          Profile/Style/Learning, greeting rules, norm(), translit_matches(), classify() tags
src/context.rs          UserContext: profile store, whoami cache, draft(), tag(), resolve_recipients(), prompt_section()
src/learn.rs            parse_sent() (reply ↔ quoted original), learn(), index_senders(), similar()
src/followup.rs         Follow-up scan and list
src/llm.rs, agent.rs    Backends (Ollama native, Anthropic Messages, OpenAI-compatible; JSON mode) + tool loop; insights()
src/settings.rs         Provider list + defaults, validation, DPAPI, export; store.rs = SQLite schema
src/transfer.rs         Settings files (export / validated import)
src/bin/outlook-selftest.rs  read-only real-Outlook checks (--bodies, --followups, --related, --llm …)
ui/                     index.html, app.css, app.js (vanilla; esc() everything; dir="auto")
ui/help/                Help pages shown in the Help dialog (iframe; F1): index, guide, config, data,
                        troubleshooting, about + help.css, img/; ui/help/ar/ = the same six pages in Arabic.
                        Embedded in the exe with the rest of ui/
```

## Outlook COM notes

- Classic Outlook only (the new Outlook has no COM). Folder names are localized: resolve default folders
  with `GetDefaultFolder` / match by **EntryID** (`FolderNode.special`), never by name.
- `folder.Items` returns a **new collection on each access**: keep it in a variable before
  `Sort`/`Restrict`/`GetFirst`. COM collections are 1-based (`Disp::items`).
- **Don't `Restrict` on dates** (locale-dependent strings break on non-English Windows): sort newest
  first and stop at the cutoff in code. Keyword search uses DASL `@SQL=... LIKE '%term%'`, `'` → `''`.
- Exchange senders have X.500 addresses: resolve SMTP via GetExchangeUser → PR_SMTP_ADDRESS.
- `ReceivedTime` carries a misleading tz; drafts have the sentinel date 4501-01-01 (treated as none).
- Folders contain meeting items (Class 53–57) and others: filter supported classes.
- `Body` can be empty for HTML-only/protected mail → `body_text()` falls back to `html_to_text(HTMLBody)`.
  **Meeting items have no `HTMLBody`**: `html_body()` reads `RTFBody` (a byte array; usually `\fromhtml1`
  wrapping the original HTML) through `rtf::to_html`. `outlook-selftest --bodies` measures this.
- Reply drafts: `Reply()/ReplyAll()`, then the styled block goes at the top of HTMLBody with
  `font-family` **and** `mso-bidi-font-family` (Word takes Arabic text's font from the bidi properties).
  Signatures are not added by COM unless the draft is displayed.
- "Already replied": `PR_LAST_VERB_EXECUTED` (0x10810003) = 102/103, 104 = forwarded.
- `INSERT OR IGNORE` burns AUTOINCREMENT values in SQLite — `short_id` selects first.
- Outlook's object-model guard may show a security prompt on some machines — IT policy, not a bug.

## LLM notes

- History is stored in a neutral format and converted per provider (`llm.rs` header), so a conversation can
  switch provider. Every tool call gets an id, even from Ollama. On an LLM error the whole turn is rolled
  back (no assistant tool call without its results).
- **Within a turn, requests only append:** the system prompt (it contains the time) and the compacted
  history prefix are computed once per turn. Claude's thinking blocks are bound to everything before
  them, so they are echoed back unchanged (`raw`) within the turn and dropped from earlier turns.
- **Anthropic:** native Messages API, `anthropic-version: 2026-10-21` (sampling parameters are rejected:
  no temperature), `max_tokens` 16000, thinking left at the model default. `fallbacks: "default"` (beta
  `server-side-fallback-2026-07-01`) only for models that take it, only on api.anthropic.com. A
  `refusal` stop reason becomes an error. Tool ids are sanitized to `[A-Za-z0-9_-]`.
- **OpenAI:** GPT-5 / o-series accept only the default temperature (omitted); tool messages carry no
  `name`; `/models` is filtered to chat models.
- **Gemini:** tool-call `extra_content` (thought signatures) must be echoed back (`extra`); empty tool
  schemas are dropped. **Kimi:** `reasoning_content` is passed back within the turn; temperature ≤ 1.
- **Ollama:** small models mangle long ids → short ids (`m1`…); `num_ctx` set explicitly; bodies are
  truncated and older tool results shortened (`compact`); `<think>…</think>` is stripped.
- Tool errors are returned as `{"error": ...}` JSON, never raised, so the model can recover. Keep the tool
  count small and descriptions short — local models degrade with many/verbose tools.

## Commands

```
cargo test --lib                         # all unit tests (no Outlook needed)
cargo run -- --demo                      # sample mailbox
cargo run --bin outlook-selftest [-- --bodies | --followups | --llm]   # read-only, counts only
cargo build --release                    # release exe; CI builds and publishes it on a v* tag
```

UI screenshots: launch with `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9333` and
Playwright `connect_over_cdp`, always with `--demo`. Native file dialogs (import/export) can be driven with
window messages to the dialog (WM_SETTEXT on edit 1001/1148, WM_COMMAND IDOK) — never with global
keystrokes or screen captures, which can reach other applications.

**Releasing:** bump `version` in `Cargo.toml` and `tauri.conf.json`, then push tag `v<version>`;
`.github/workflows/release.yml` tests, builds and attaches `InboxChat.exe` (+ SHA-256) to the release.
