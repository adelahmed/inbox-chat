(() => {
  const $ = (id) => document.getElementById(id);
  const invoke = (cmd, args) => window.__TAURI__.core.invoke(cmd, args);
  const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
  const errText = (e) => (typeof e === "string" ? e : (e && e.message) || String(e));
  const store = {
    get(k, d) { try { const v = localStorage.getItem(k); return v === null ? d : JSON.parse(v); } catch (_) { return d; } },
    set(k, v) { try { localStorage.setItem(k, JSON.stringify(v)); } catch (_) { /* storage unavailable */ } },
  };

  // ------------------------------------------------------------------ helpers
  function fmtWhen(iso, long) {
    if (!iso) return "";
    const d = new Date(iso);
    if (isNaN(d)) return "";
    const now = new Date();
    const time = d.toLocaleTimeString("en-GB", { hour: "2-digit", minute: "2-digit" });
    if (long) return d.toLocaleDateString("en-GB", { weekday: "short", day: "numeric", month: "short", year: "numeric" }) + " " + time;
    if (d.toDateString() === now.toDateString()) return time;
    const days = (now - d) / 864e5;
    if (days < 6) return d.toLocaleDateString("en-GB", { weekday: "short" }) + " " + time;
    if (d.getFullYear() === now.getFullYear()) return d.toLocaleDateString("en-GB", { day: "numeric", month: "short" });
    return d.toLocaleDateString("en-GB");
  }
  const fmtSize = (n) => (n >= 1048576 ? (n / 1048576).toFixed(1) + " MB" : n >= 1024 ? Math.round(n / 1024) + " KB" : n + " B");
  const initials = (name) => (String(name || "?").trim().split(/\s+/).slice(0, 2).map((w) => w[0]).join("") || "?").toUpperCase();
  const extOf = (name) => (String(name).match(/\.([a-z0-9]+)$/i) || ["", ""])[1].toLowerCase();
  const PREVIEWABLE = new Set(["png", "jpg", "jpeg", "gif", "bmp", "webp", "pdf", "txt", "csv", "log", "md", "json", "xml", "ics", "vcf", "tsv"]);
  function extClass(ext) {
    if (ext === "pdf") return "pdf";
    if (/^docx?$|^rtf$|^odt$/.test(ext)) return "doc";
    if (/^xlsx?$|^xlsm$|^csv$|^ods$/.test(ext)) return "xls";
    if (/^pptx?$|^odp$/.test(ext)) return "ppt";
    if (/^(png|jpe?g|gif|bmp|webp|svg|tiff?)$/.test(ext)) return "img";
    if (/^(zip|rar|7z|gz)$/.test(ext)) return "zip";
    return "";
  }

  let toastTimer = null;
  function toast(text, bad) {
    let t = $("toast");
    if (!t) {
      t = document.createElement("div"); t.id = "toast";
      t.style.cssText = "position:fixed;left:50%;bottom:24px;transform:translateX(-50%);max-width:70vw;padding:9px 16px;border-radius:8px;font-size:13px;z-index:50;box-shadow:0 8px 24px rgba(0,0,0,.18)";
      document.body.appendChild(t);
    }
    t.textContent = text;
    t.style.background = bad ? "#b91c1c" : "#1d2433"; t.style.color = "#fff"; t.hidden = false;
    clearTimeout(toastTimer); toastTimer = setTimeout(() => { t.hidden = true; }, bad ? 7000 : 3500);
  }

  // ------------------------------------------------------------------ resizable panes
  for (const v of ["--w-folders", "--w-list", "--w-chat"]) {
    const w = store.get("pane" + v, null);
    if (w) document.documentElement.style.setProperty(v, w + "px");
  }
  document.querySelectorAll(".splitter").forEach((sp) => {
    sp.addEventListener("pointerdown", (e) => {
      const v = sp.dataset.var, min = +sp.dataset.min, max = +sp.dataset.max, invert = !!sp.dataset.invert;
      const start = parseInt(getComputedStyle(document.documentElement).getPropertyValue(v)) || 300, x0 = e.clientX;
      sp.setPointerCapture(e.pointerId); sp.classList.add("drag");
      const move = (ev) => {
        const w = Math.max(min, Math.min(max, start + (invert ? x0 - ev.clientX : ev.clientX - x0)));
        document.documentElement.style.setProperty(v, w + "px");
      };
      const up = () => {
        sp.removeEventListener("pointermove", move); sp.classList.remove("drag");
        store.set("pane" + v, parseInt(getComputedStyle(document.documentElement).getPropertyValue(v)));
        fitBody();
      };
      sp.addEventListener("pointermove", move);
      sp.addEventListener("pointerup", up, { once: true });
    });
  });

  // ------------------------------------------------------------------ folders
  const SPECIAL_ORDER = ["inbox", "drafts", "sent", "deleted", "junk", "outbox"];
  const SPECIAL_ICON = { inbox: "📥", drafts: "📝", sent: "📤", deleted: "🗑", junk: "⚠", outbox: "📮" };
  const expanded = new Set(store.get("expanded", []));
  let currentFolder = null;   // {path, name, special}

  function sortNodes(nodes) {
    const rank = (n) => (n.special ? SPECIAL_ORDER.indexOf(n.special) : 99);
    return [...nodes].sort((a, b) => rank(a) - rank(b) || a.name.localeCompare(b.name));
  }

  function renderTree(nodes, parent) {
    const ul = document.createElement("ul");
    for (const n of sortNodes(nodes)) {
      const li = document.createElement("li");
      const row = document.createElement("div");
      const open = expanded.has(n.path);
      row.className = "node" + (n.unread ? " has-unread" : "") + (currentFolder && currentFolder.path === n.path ? " active" : "");
      row.dataset.path = n.path;
      row.title = n.path;
      row.innerHTML = `<span class="tw">${n.children.length ? (open ? "▾" : "▸") : ""}</span>` +
        `<span class="ic">${SPECIAL_ICON[n.special] || "📁"}</span><span class="nm" dir="auto"></span>` +
        `<span class="ct">${n.special === "drafts" ? (n.total || "") : (n.unread || "")}</span>`;
      row.querySelector(".nm").textContent = n.name;
      row.addEventListener("click", (e) => {
        if (e.target.closest(".tw") && n.children.length) {
          open ? expanded.delete(n.path) : expanded.add(n.path);
          store.set("expanded", [...expanded]);
          renderFolders();
          return;
        }
        selectFolder(n);
      });
      li.appendChild(row);
      if (n.children.length && open) li.appendChild(renderTree(n.children, li));
      ul.appendChild(li);
    }
    return ul;
  }

  let folderData = [];
  function renderFolders() { $("folder-tree").replaceChildren(renderTree(folderData)); }

  async function loadFolders(selectDefault) {
    if (!folderData.length) $("folder-tree").innerHTML = '<div class="loading">Connecting to Outlook…</div>';
    try {
      folderData = await invoke("folders");
      renderFolders();
      if (selectDefault) {
        const inbox = folderData.find((n) => n.special === "inbox");
        selectFolder(inbox || { path: "inbox", name: "Inbox", special: "inbox" });
      }
    } catch (e) {
      $("folder-tree").innerHTML = `<div class="pane-error">${esc(errText(e))}<br><button class="ghost" type="button">Try again</button></div>`;
      $("folder-tree").querySelector("button").addEventListener("click", () => loadFolders(selectDefault));
    }
  }
  $("refresh-folders").addEventListener("click", () => loadFolders(false));

  // ------------------------------------------------------------------ message list
  let listOffset = 0, listToken = 0, selectedId = null;

  function selectFolder(n) {
    currentFolder = n;
    $("list-title").textContent = n.name;
    document.querySelectorAll(".node").forEach((el) => el.classList.toggle("active", el.dataset.path === n.path));
    loadList(false);
  }

  const TAG_LABEL = { to_you: "To you", cc_you: "CC", mentions_you: "Mentions you", department: "Department", request: "Request" };
  const isForYou = (tags) => (tags || []).some((t) => t === "to_you" || t === "mentions_you" || t === "department");
  const tagsHtml = (tags) => (tags || []).map((t) => `<span class="tag ${esc(t)}">${esc(TAG_LABEL[t] || t)}</span>`).join("");
  function applyForMe() {
    const on = $("for-me").checked;
    document.querySelectorAll(".mi").forEach((el) => { el.hidden = on && !el.classList.contains("for-you"); });
  }
  $("for-me").addEventListener("change", () => { store.set("forMe", $("for-me").checked); applyForMe(); });
  $("for-me").checked = store.get("forMe", false);

  function renderItem(s) {
    const el = document.createElement("div");
    el.className = "mi" + (s.unread ? " unread" : "") + (s.id === selectedId ? " active" : "") + (isForYou(s.tags) ? " for-you" : "");
    el.dataset.id = s.id;
    el.setAttribute("role", "option");
    el.innerHTML = `<div class="r1"><span class="from" dir="auto"></span><span class="when"></span></div>` +
      `<div class="subj" dir="auto"></div><div class="prev" dir="auto"></div>`;
    el.querySelector(".from").textContent = s.from_name || s.from || "(unknown sender)";
    el.querySelector(".when").textContent = fmtWhen(s.received);
    const subj = el.querySelector(".subj");
    if (s.importance === "high") subj.insertAdjacentHTML("afterbegin", '<span class="imp">! </span>');
    subj.appendChild(document.createTextNode(s.subject));
    if (s.attachments) subj.insertAdjacentHTML("beforeend", `<span class="flags">📎${s.attachments > 1 ? s.attachments : ""}</span>`);
    el.querySelector(".prev").textContent = s.preview || "";
    if (s.tags && s.tags.length) el.insertAdjacentHTML("beforeend", `<div class="r3 tags">${tagsHtml(s.tags)}</div>`);
    el.addEventListener("click", () => openEmail(s.id));
    return el;
  }

  async function loadList(append) {
    const box = $("message-list"), token = ++listToken;
    if (!append) { listOffset = 0; box.innerHTML = '<div class="loading">Loading…</div>'; }
    box.querySelector(".more")?.remove();
    try {
      const page = await invoke("messages", { folder: currentFolder.path, offset: listOffset });
      if (token !== listToken) return;
      if (!append) box.replaceChildren();
      page.items.forEach((s) => box.appendChild(renderItem(s)));
      applyForMe();
      listOffset += page.items.length;
      if (!box.children.length) box.innerHTML = '<div class="list-empty">No emails in this folder.</div>';
      if (page.has_more) {
        const b = document.createElement("button");
        b.className = "ghost more"; b.type = "button"; b.textContent = "Load more";
        b.addEventListener("click", () => loadList(true));
        box.appendChild(b);
      }
    } catch (e) {
      if (token !== listToken) return;
      box.innerHTML = `<div class="pane-error">${esc(errText(e))}</div>`;
    }
  }
  $("refresh-list").addEventListener("click", () => currentFolder && loadList(false));
  $("message-list").addEventListener("keydown", (e) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    const items = [...document.querySelectorAll(".mi")];
    const i = items.findIndex((el) => el.dataset.id === selectedId);
    const next = items[e.key === "ArrowDown" ? Math.min(items.length - 1, i + 1) : Math.max(0, i - 1)];
    if (next) { e.preventDefault(); openEmail(next.dataset.id); next.scrollIntoView({ block: "nearest" }); }
  });

  // ------------------------------------------------------------------ reading pane
  let readToken = 0, current = null;

  function bodyDoc(m) {
    let html = m.html || "";
    let blocked = 0;
    if (html.trim()) {
      html = html.replace(/<script\b[\s\S]*?<\/script\s*>/gi, "")
                 .replace(/(<img\b[^>]*?\ssrc\s*=\s*["']?)cid:[^"'\s>]*/gi, "$1data:,");
      blocked = (html.match(/<img\b[^>]*?\ssrc\s*=\s*["']?https?:/gi) || []).length;
    } else {
      html = `<pre dir="auto" style="white-space:pre-wrap;font:inherit;margin:0">${esc(m.text)}</pre>`;
    }
    // A strict policy inside the frame too: no scripts, no remote content (like Outlook's
    // "download pictures" off), inline styles allowed for email formatting.
    // Tables keep the widths the sender gave them (as in Outlook) and scroll sideways when wider than
    // the pane; "overflow-wrap:anywhere" or a max-width would squeeze their columns to a letter or two.
    const head = `<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src data:; style-src 'unsafe-inline'; font-src data:">` +
      `<base target="_blank"><style>html,body{margin:0;padding:0;font:14px/1.5 "Segoe UI",Calibri,Tahoma,Arial,sans-serif;color:#1d2433}` +
      `html{overflow-x:auto}body{padding:4px 2px;overflow-wrap:break-word}img{max-width:100%;height:auto}</style>`;
    return { doc: `<!DOCTYPE html><html><head>${head}</head><body dir="auto">${html}</body></html>`, blocked };
  }

  function fitBody() {
    const f = document.querySelector(".rd-body iframe");
    try {
      const d = f && f.contentDocument;
      const el = d && d.documentElement;
      // Room for the sideways scrollbar when a wide table overflows the pane.
      if (el) f.style.height = Math.max(60, el.scrollHeight + (el.scrollWidth > el.clientWidth ? 22 : 4)) + "px";
    } catch (_) { /* not ready */ }
  }
  window.addEventListener("resize", fitBody);

  function attachmentChip(m, a) {
    const ext = extOf(a.name), can = PREVIEWABLE.has(ext);
    const el = document.createElement("div");
    el.className = "att";
    el.innerHTML = `<span class="ext ${extClass(ext)}">${esc(ext || "file")}</span>` +
      `<div class="an"><div class="n" dir="auto"></div><div class="s">${esc(fmtSize(a.size))}</div></div><div class="acts"></div>`;
    el.querySelector(".n").textContent = a.name; el.title = a.name;
    const acts = el.querySelector(".acts");
    if (can) {
      const p = document.createElement("button"); p.type = "button"; p.textContent = "Preview";
      p.addEventListener("click", () => previewAttachment(m.id, a)); acts.appendChild(p);
    }
    const o = document.createElement("button"); o.type = "button"; o.textContent = "Open";
    o.title = "Open in its app (e.g. PowerPoint). Opens in Protected View, like Outlook.";
    o.addEventListener("click", () => openAttachment(m.id, a)); acts.appendChild(o);
    return el;
  }

  function renderReader(m) {
    const r = $("reader");
    r.replaceChildren();
    const tools = document.createElement("div");
    tools.className = "rd-tools";
    const btn = (label, cls, fn, title) => {
      const b = document.createElement("button"); b.type = "button"; b.className = "ghost " + (cls || ""); b.textContent = label;
      if (title) b.title = title;
      b.addEventListener("click", fn); tools.appendChild(b);
    };
    if (!m.is_draft) {
      btn("✨ Draft reply", "ai", () => prefill(`Draft a reply to [${m.id}]: `), "The assistant reads the thread and saves a reply in Drafts");
      btn("✨ Draft reply all", "ai", () => prefill(`Draft a reply-all to [${m.id}]: `));
      btn("✨ Forward", "ai", () => prefill(`Forward [${m.id}] to `), "Saves a forward (with attachments) in Drafts");
      btn("✨ Summarize thread", "ai", () => ask(`Summarize the thread of [${m.id}] and list what needs my action.`));
    }
    btn(m.is_draft ? "Open in Outlook to edit / send" : "Open in Outlook", "", async () => {
      try { await invoke("open_in_outlook", { id: m.id }); } catch (e) { toast(errText(e), true); }
    }, "Opens this item in Outlook's own window");
    r.appendChild(tools);

    const ins = document.createElement("div");
    ins.className = "insights"; ins.id = "insights";
    r.appendChild(ins);

    const head = document.createElement("div");
    head.className = "rd-head";
    head.innerHTML = `<h2 class="rd-subject" dir="auto"></h2>
      <div class="rd-from"><div class="avatar"></div>
        <div class="rd-meta"><div><span class="nm" dir="auto"></span> <span class="addr"></span></div>
          <div class="line to" dir="auto"></div><div class="line cc" dir="auto"></div></div>
        <div class="rd-date"></div></div>`;
    const subj = head.querySelector(".rd-subject");
    if (m.is_draft) subj.insertAdjacentHTML("afterbegin", '<span class="rd-draft">DRAFT</span>');
    subj.appendChild(document.createTextNode(m.subject));
    head.querySelector(".avatar").textContent = initials(m.from_name || m.from);
    head.querySelector(".nm").textContent = m.from_name || m.from;
    head.querySelector(".addr").textContent = m.from && m.from !== m.from_name ? `<${m.from}>` : "";
    head.querySelector(".to").textContent = m.to ? "To: " + m.to : "";
    head.querySelector(".cc").textContent = m.cc ? "Cc: " + m.cc : "";
    head.querySelector(".rd-date").textContent = fmtWhen(m.received, true);
    if (m.last_action) {
      // Outlook's "You replied/forwarded on …" status.
      const icon = m.last_action === "forwarded" ? "→" : "↩";
      head.insertAdjacentHTML("beforeend", `<div class="rd-status">${icon} You ${esc(m.last_action)}${m.last_action_at ? " · " + esc(fmtWhen(m.last_action_at, true)) : ""}</div>`);
    }
    r.appendChild(head);

    if (m.attachments.length) {
      const atts = document.createElement("div");
      atts.className = "atts";
      m.attachments.forEach((a) => atts.appendChild(attachmentChip(m, a)));
      r.appendChild(atts);
    }

    const { doc, blocked } = bodyDoc(m);
    if (blocked) {
      const note = document.createElement("div");
      note.className = "blocked-note";
      note.textContent = `${blocked} external picture${blocked > 1 ? "s" : ""} not downloaded, to protect your privacy. Open in Outlook to see them.`;
      r.appendChild(note);
    }
    const body = document.createElement("div");
    body.className = "rd-body";
    const frame = document.createElement("iframe");
    frame.setAttribute("sandbox", "allow-same-origin"); // no scripts, forms or navigation
    frame.setAttribute("title", "Email body");
    frame.srcdoc = doc;
    frame.addEventListener("load", () => {
      fitBody();
      setTimeout(fitBody, 300);
      try {
        frame.contentDocument.addEventListener("click", (e) => {
          const a = e.target.closest && e.target.closest("a[href]");
          if (!a) return;
          e.preventDefault();
          const href = a.getAttribute("href") || "";
          if (/^https?:\/\//i.test(href)) {
            if (confirm(`Open this link in your browser?\n\n${href}`)) invoke("open_url", { url: href }).catch((er) => toast(errText(er), true));
          } else if (/^mailto:/i.test(href)) {
            prefill(`Draft a new email to ${href.replace(/^mailto:/i, "").split("?")[0]}: `);
          }
        });
      } catch (_) { /* ignore */ }
    });
    body.appendChild(frame);
    r.appendChild(body);

    const hist = document.createElement("div");
    hist.className = "history"; hist.id = "history";
    hist.innerHTML = '<h3>Conversation history</h3><div class="loading">Loading…</div>';
    r.appendChild(hist);
    r.scrollTop = 0;
  }

  function renderHistory(msgs, currentId) {
    const h = $("history");
    if (!h) return;
    if (msgs.length <= 1) { h.remove(); return; }
    h.innerHTML = `<h3>Conversation history · ${msgs.length} messages</h3>`;
    for (const t of [...msgs].reverse()) {
      const el = document.createElement("div");
      el.className = "hm" + (t.id === currentId ? " current" : "");
      el.innerHTML = `<div class="hm-head"><span class="nm" dir="auto"></span><span class="dt"></span></div><div class="hm-body" dir="auto"></div>`;
      el.querySelector(".nm").textContent = (t.from_name || t.from) + (t.id === currentId ? "  (this email)" : "");
      el.querySelector(".dt").textContent = fmtWhen(t.received, true);
      el.querySelector(".hm-body").textContent = t.body || "(no text)";
      el.querySelector(".hm-head").title = "Open this message";
      el.querySelector(".hm-head").addEventListener("click", () => { if (t.id !== currentId) openEmail(t.id); });
      if ((t.body || "").split("\n").length > 5 || (t.body || "").length > 400) {
        const more = document.createElement("button");
        more.className = "hm-more"; more.type = "button"; more.textContent = "Show more";
        more.addEventListener("click", () => { el.classList.toggle("open"); more.textContent = el.classList.contains("open") ? "Show less" : "Show more"; });
        el.appendChild(more);
      }
      h.appendChild(el);
    }
  }

  // ------------------------------------------------------------------ ✨ insights panel
  let insToken = 0, insTimer = null, profileCache = null;

  function relRow(m) {
    if (!(m.tags || []).length) return "";
    const matched = (m.matched || []).length ? ` <span dir="auto">· ${esc(m.matched.join("، "))}</span>` : "";
    return `<div class="rel">${tagsHtml(m.tags)}${matched}</div>`;
  }

  function insIdle(box, m) {
    box.innerHTML = `<div class="ins-head">✨ Assistant<span class="meta"></span><button type="button">Summarize &amp; suggest reply</button></div>` +
      (relRow(m) ? `<div class="ins-body">${relRow(m)}</div>` : "");
    box.querySelector("button").addEventListener("click", () => startInsights(m, false, true));
  }

  function startInsights(m, force, now) {
    const box = $("insights");
    if (!box) return;
    const auto = !profileCache || profileCache.profile.learning.auto_suggest;
    if (!auto && !now && !force) { insIdle(box, m); return; }
    box.innerHTML = `<div class="ins-head">✨ Assistant<span class="meta"></span></div><div class="ins-loading">Reading the thread…</div>`;
    const token = ++insToken;
    clearTimeout(insTimer);
    insTimer = setTimeout(async () => {
      try {
        const i = await invoke("insights", { id: m.id, force: !!force });
        if (token === insToken && $("insights")) renderInsights($("insights"), m, i);
      } catch (e) {
        if (token !== insToken || !$("insights")) return;
        const b = $("insights");
        b.innerHTML = `<div class="ins-head">✨ Assistant<span class="meta"></span><button type="button">Try again</button></div>` +
          `<div class="ins-error" dir="auto">${esc(errText(e))}</div>`;
        b.querySelector("button").addEventListener("click", () => startInsights(m, true));
      }
    }, force || now ? 0 : 500);
  }

  function renderInsights(box, m, i) {
    const st = (profileCache && profileCache.profile.style) || {};
    const meta = [i.provider && llmInfo ? llmInfo.label : i.provider, i.model, i.cached ? "saved" : ""].filter(Boolean).join(" · ");
    let html = `<div class="ins-head">✨ Assistant<span class="meta">${esc(meta)}</span>` +
      `<button type="button" data-a="regen" title="Ask again">Regenerate</button><button type="button" data-a="toggle">Hide</button></div>` +
      `<div class="ins-body">${relRow(m)}`;
    html += `<h4>Summary</h4><p dir="auto">${esc(i.summary || "—")}</p>`;
    html += `<h4>What you need to do</h4>` + (i.actions.length
      ? `<ul dir="auto">${i.actions.map((a) => `<li>${esc(a)}</li>`).join("")}</ul>` : `<p class="muted" dir="auto">Nothing for you to do.</p>`);
    if (i.needs_reply && i.reply) {
      const basis = i.examples_used ? `based on your past replies` : `in your style`;
      const font = st.enabled ? `font-family:'${String(st.font_family || "").replace(/['";<>]/g, "")}',Arial;` : "";
      html += `<h4>Suggested reply <span style="text-transform:none;letter-spacing:0">· ${basis}</span></h4>
        <div class="reply-box">
          ${st.enabled ? `<div class="gl" dir="rtl">${esc((st.greeting_single || "").split("{name}")[0])}<input type="text" class="gname" dir="auto" title="How to address the recipient"></div>
            <div class="gl" style="font-size:11px">Greeting and sign-off are finalised from the actual recipients when saved («للجميع» for several).</div>` : ""}
          <textarea class="rbody" dir="${st.enabled && st.rtl ? "rtl" : "auto"}" style="${font}"></textarea>
          ${st.enabled ? `<div class="sl" dir="rtl" style="white-space:pre-line">${esc(st.signoff || "")}</div>` : ""}
          <div class="reply-actions">
            <button type="button" class="primary" data-a="save">Save to Drafts</button>
            <button type="button" class="ghost" data-a="saveall">Save as Reply all</button>
            <button type="button" class="ghost" data-a="chat">Refine in chat</button>
          </div>
        </div>`;
    }
    html += `</div>`;
    box.innerHTML = html;
    const ta = box.querySelector(".rbody");
    if (ta) { ta.value = i.reply; ta.style.height = Math.min(320, Math.max(90, ta.scrollHeight + 4)) + "px"; }
    const gn = box.querySelector(".gname");
    if (gn) gn.value = i.greeting_name || "";
    box.querySelectorAll("[data-a]").forEach((b) => b.addEventListener("click", async () => {
      const a = b.dataset.a;
      if (a === "regen") return startInsights(m, true);
      if (a === "toggle") { const body = box.querySelector(".ins-body"); body.hidden = !body.hidden; b.textContent = body.hidden ? "Show" : "Hide"; return; }
      if (a === "chat") return prefill(`Draft a reply to [${m.id}]. Start from this text and improve it:\n${ta.value}\n\nChanges: `);
      b.disabled = true;
      try {
        const res = await invoke("save_reply", { id: m.id, body: ta.value, greetingName: gn ? gn.value : null, replyAll: a === "saveall" });
        toast(`Saved in Outlook Drafts as ${res.id} — nothing was sent.`);
        markReadInUi(m.id);
        if (currentFolder && currentFolder.special === "drafts") loadList(false);
      } catch (e) { toast(errText(e), true); } finally { b.disabled = false; }
    }));
  }

  /// The email is now read in Outlook: un-bold it in the list and refresh the folder counts.
  function markReadInUi(id) {
    document.querySelectorAll(`.mi[data-id="${CSS.escape(id)}"]`).forEach((el) => el.classList.remove("unread"));
    loadFolders(false);
  }

  async function openEmail(id) {
    selectedId = id;
    document.querySelectorAll(".mi").forEach((el) => el.classList.toggle("active", el.dataset.id === id));
    const token = ++readToken;
    $("reader").innerHTML = '<div class="placeholder">Opening…</div>';
    try {
      const m = await invoke("open_message", { id });
      if (token !== readToken) return;
      current = m;
      renderReader(m);
      setContext(m);
      startInsights(m);
      if (m.marked_read) markReadInUi(id);
      const thread = await invoke("message_thread", { id }).catch(() => []);
      if (token === readToken) renderHistory(thread, id);
    } catch (e) {
      if (token === readToken) $("reader").innerHTML = `<div class="pane-error">${esc(errText(e))}</div>`;
    }
  }

  // ------------------------------------------------------------------ attachments
  const viewer = $("viewer");
  let viewerUrl = null, viewerAtt = null;
  function closeViewer() {
    viewer.close();
    if (viewerUrl) { URL.revokeObjectURL(viewerUrl); viewerUrl = null; }
    $("viewer-body").replaceChildren();
  }
  $("viewer-close").addEventListener("click", closeViewer);
  viewer.addEventListener("cancel", (e) => { e.preventDefault(); closeViewer(); });
  $("viewer-open").addEventListener("click", () => viewerAtt && openAttachment(viewerAtt.id, viewerAtt.a));

  async function openAttachment(id, a) {
    toast(`Opening ${a.name}…`);
    try { await invoke("attachment_open", { id, index: a.index }); } catch (e) { toast(errText(e), true); }
  }

  async function previewAttachment(id, a) {
    viewerAtt = { id, a };
    $("viewer-title").textContent = a.name;
    const body = $("viewer-body");
    body.innerHTML = '<div class="msg-box">Loading preview…</div>';
    viewer.showModal();
    try {
      const p = await invoke("attachment_preview", { id, index: a.index });
      if (!p.previewable) { body.innerHTML = `<div class="msg-box">${esc(p.reason || "No preview for this file type.")}<br>Use “Open in app”.</div>`; return; }
      const bin = Uint8Array.from(atob(p.data), (c) => c.charCodeAt(0));
      if (p.mime.startsWith("image/")) {
        const img = document.createElement("img"); img.alt = a.name;
        img.src = `data:${p.mime};base64,${p.data}`;
        body.replaceChildren(img);
      } else if (p.mime === "application/pdf") {
        viewerUrl = URL.createObjectURL(new Blob([bin], { type: "application/pdf" }));
        const f = document.createElement("iframe"); f.title = a.name; f.src = viewerUrl;
        body.replaceChildren(f);
      } else {
        const pre = document.createElement("pre"); pre.dir = "auto";
        pre.textContent = new TextDecoder("utf-8").decode(bin);
        body.replaceChildren(pre);
      }
    } catch (e) {
      body.innerHTML = `<div class="msg-box error">${esc(errText(e))}</div>`;
    }
  }

  // ------------------------------------------------------------------ chat
  const thread = $("thread"), input = $("input"), send = $("send"), scroller = $("scroller");
  let conversationId = null, busy = false, llmInfo = null, viewing = null, conversations = [];

  function setContext(m) {
    viewing = m ? { id: m.id, subject: m.subject } : null;
    $("context").hidden = !viewing;
    if (viewing) { $("context-ref").textContent = viewing.id; $("context-ref").dataset.ref = viewing.id; $("context-subject").textContent = viewing.subject; }
    input.placeholder = viewing ? `About ${viewing.id}: reply, forward to…, summarize… (or ask about anything)` : "Ask, or say what to draft…";
    if ($("empty")) showEmpty();
  }
  $("context-clear").addEventListener("click", () => setContext(null));

  function prefill(text) {
    input.value = text; autosize(); input.focus();
    input.setSelectionRange(text.length, text.length);
  }

  function md(text) {
    const lines = esc(text).split("\n");
    let html = "", list = null, para = [];
    const inline = (s) => s
      .replace(/`([^`]+)`/g, "<code>$1</code>")
      .replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>")
      .replace(/\[?\b(m\d{1,6})\b\]?/g, '<span class="ref" data-ref="$1" title="Open this email">$1</span>');
    const flushPara = () => { if (para.length) { html += `<p dir="auto">${inline(para.join("<br>"))}</p>`; para = []; } };
    const flushList = () => { if (list) { html += `</${list}>`; list = null; } };
    for (const raw of lines) {
      const line = raw.trimEnd();
      let m;
      if (!line.trim()) { flushPara(); flushList(); continue; }
      if ((m = line.match(/^#{1,4}\s+(.*)/))) { flushPara(); flushList(); html += `<h3 dir="auto">${inline(m[1])}</h3>`; continue; }
      if ((m = line.match(/^\s*[-*•]\s+(.*)/)) || (m = line.match(/^\s*\d+[.)]\s+(.*)/))) {
        flushPara();
        const type = /^\s*\d/.test(line) ? "ol" : "ul";
        if (list !== type) { flushList(); html += `<${type} dir="auto">`; list = type; }
        html += `<li>${inline(m[1])}</li>`;
        continue;
      }
      flushList(); para.push(line);
    }
    flushPara(); flushList();
    return html;
  }
  const userHtml = (text, about) => (about ? `<div class="about-tag">about <span class="ref" data-ref="${esc(about)}">${esc(about)}</span></div>` : "") +
    `<p dir="auto">${esc(text).replace(/\[?\b(m\d{1,6})\b\]?/g, '<span class="ref" data-ref="$1">$1</span>').replace(/\n/g, "<br>")}</p>`;

  function showEmpty() {
    thread.replaceChildren($("empty-tpl").content.cloneNode(true));
    if (viewing) {
      // Suggestions for the open email.
      $("empty").querySelector("h2").textContent = "What should I do with this email?";
      $("empty").querySelector(".chips").innerHTML =
        ["Summarize this email", "What do I need to do?", "Draft a reply", "لخّص هذه الرسالة وما المطلوب مني"]
          .map((t) => `<button class="chip" type="button" dir="auto">${esc(t)}</button>`).join("") +
        `<button class="chip" type="button" data-prefill="Forward this email to ">Forward to…</button>`;
    }
    updatePrivacy();
  }

  function add(role, html, steps) {
    $("empty")?.remove();
    const row = document.createElement("div");
    row.className = `msg ${role}`;
    let inner = "";
    if (steps && steps.length) inner += '<div class="steps">' + steps.map((s) => `<span class="step${s.ok ? "" : " err"}">${esc(s.label)}</span>`).join("") + "</div>";
    row.innerHTML = `<div class="bubble" dir="auto">${inner}${html}</div>`;
    thread.appendChild(row);
    scroller.scrollTop = scroller.scrollHeight;
    return row;
  }

  /// `general`: a mailbox-wide question (e.g. the Follow-up button) — not about the open email.
  async function ask(text, general) {
    if (busy || !text.trim()) return;
    busy = true; send.disabled = true;
    const about = general ? null : viewing && viewing.id;
    add("user", userHtml(text, about));
    input.value = ""; autosize();
    const pending = add("assistant", '<span class="typing">Working through your mailbox…</span>');
    try {
      const data = await invoke("chat", { conversationId, message: text, viewing: about });
      pending.remove();
      const row = add("assistant", md(data.reply || ""), data.steps);
      if (data.followups) renderFollowups(row, data.followups);
      conversationId = data.conversation_id;
      await refreshConversations();
      const steps = data.steps || [];
      if (steps.some((s) => s.ok && /draft/.test(s.tool))) toast("Saved in Outlook Drafts — nothing was sent.");
      // Emails the assistant read or acted on are now marked read in Outlook: refresh counts and the list.
      if (steps.some((s) => s.ok && /draft|read_/.test(s.tool))) {
        loadFolders(false);
        if (currentFolder) loadList(false);
      }
    } catch (e) {
      pending.remove();
      add("assistant", `<p class="error" dir="auto">${esc(errText(e))}</p>`);
    } finally {
      busy = false; send.disabled = false; input.focus();
    }
  }

  // ------------------------------------------------------------------ follow-ups
  const FOLLOWUP_PROMPT = "What emails do I need to follow up on from the last 10 days? Check what I sent, to whom, and who hasn't answered yet.";

  /// The follow-up list under an assistant answer: who, what was asked, how long it has waited, Open / Dismiss.
  /// Only requests still waiting are shown (answered ones need nothing).
  function renderFollowups(row, f) {
    const bubble = row.querySelector(".bubble");
    const card = document.createElement("div");
    card.className = "fu-card";
    const items = (f.open || []).filter((i) => !i.status || i.status === "followup:open"); // older saved lists had tags
    const line = (i) => {
      const el = document.createElement("div");
      el.className = "fu-item";
      const days = i.days_waiting ?? 0;
      el.innerHTML = `<div class="fu-top"><span class="fu-who" dir="auto"></span>
          <span class="fu-wait" title="Waiting since ${esc(fmtWhen(i.sent_at, true))}">⏳ ${days === 0 ? "today" : days === 1 ? "1 day" : days + " days"}</span></div>
        <div class="fu-subj" dir="auto"></div><div class="fu-asked" dir="auto"></div><div class="fu-note" dir="auto"></div>
        <div class="fu-acts"><button type="button" class="ghost small" data-a="open">Open sent email</button>
          <button type="button" class="ghost small" data-a="dismiss" title="Hide from future follow-up lists">Dismiss</button></div>`;
      el.querySelector(".fu-who").textContent = (i.to || []).join("، ");
      el.querySelector(".fu-subj").textContent = i.subject;
      el.querySelector(".fu-asked").textContent = i.asked;
      const notes = [];
      if (i.you_chased_on) notes.push(`You followed up on ${fmtWhen(i.you_chased_on)}`);
      if (i.others_replied) notes.push(`Others wrote: ${i.others_replied.join("، ")}`);
      el.querySelector(".fu-note").textContent = notes.join(" · ");
      el.querySelector("[data-a=open]").addEventListener("click", () => openEmail(i.id));
      el.querySelector("[data-a=dismiss]").addEventListener("click", async () => {
        try {
          const r = await invoke("dismiss_followup", { id: i.id });
          el.remove(); updateFooter(r.dismissed_count);
          toastAction(`Dismissed “${i.subject}”`, "Undo", async () => {
            const back = await invoke("dismiss_followup", { id: i.id, dismissed: false });
            card.insertBefore(el, card.querySelector(".fu-foot")); updateFooter(back.dismissed_count);
          });
        } catch (e) { toast(errText(e), true); }
      });
      return el;
    };
    card.innerHTML = `<div class="fu-head">Waiting for an answer · last ${esc(f.window_days)} days · <b>${items.length}</b></div>`;
    if (!items.length) card.insertAdjacentHTML("beforeend", '<div class="fu-empty">Nothing waiting on anyone.</div>');
    items.forEach((i) => card.appendChild(line(i)));
    const foot = document.createElement("div");
    foot.className = "fu-foot";
    card.appendChild(foot);
    function updateFooter(n) {
      foot.innerHTML = n ? `${n} dismissed · <button type="button" class="linkish">Restore all</button>` : "";
      foot.querySelector("button")?.addEventListener("click", async () => {
        try { await invoke("restore_followups"); updateFooter(0); toast("Restored. Ask for follow-ups again to see them."); }
        catch (e) { toast(errText(e), true); }
      });
    }
    updateFooter(f.dismissed_count || 0);
    bubble.appendChild(card);
    scroller.scrollTop = scroller.scrollHeight;
  }

  /// A toast with one action button (e.g. Undo).
  function toastAction(text, label, fn) {
    toast(text);
    const t = $("toast");
    const b = document.createElement("button");
    b.type = "button"; b.textContent = label; b.className = "toast-btn";
    b.addEventListener("click", () => { t.hidden = true; fn().catch((e) => toast(errText(e), true)); });
    t.appendChild(b);
  }

  $("followup-btn").addEventListener("click", () => ask(FOLLOWUP_PROMPT, true));

  function autosize() {
    input.style.height = "auto";
    input.style.height = Math.min(input.scrollHeight + 2, 180) + "px";
    input.classList.toggle("scroll", input.scrollHeight > 180);
  }
  $("form").addEventListener("submit", (e) => { e.preventDefault(); ask(input.value); });
  input.addEventListener("input", autosize);
  input.addEventListener("keydown", (e) => { if (e.key === "Enter" && !e.shiftKey && !e.isComposing) { e.preventDefault(); ask(input.value); } });
  document.addEventListener("click", (e) => {
    const chip = e.target.closest(".chip");
    if (chip) return chip.dataset.prefill ? prefill(chip.dataset.prefill) : ask(chip.textContent);
    const ref = e.target.closest(".ref[data-ref]");
    if (ref) openEmail(ref.dataset.ref);
  });

  async function refreshConversations() {
    try { conversations = await invoke("list_conversations"); } catch (_) { conversations = []; }
    const sel = $("conv-select");
    sel.replaceChildren(new Option("New conversation", ""),
      ...conversations.map((c) => new Option(`${c.title}  ·  ${fmtWhen(c.updated)}`, c.id)));
    sel.value = conversationId || "";
    $("delete-chat").disabled = !conversationId;
  }

  async function openConversation(id) {
    if (busy) return;
    try {
      const conv = await invoke("load_conversation", { id });
      conversationId = id;
      thread.replaceChildren();
      for (const m of conv.messages) {
        if (m.role === "user") add("user", userHtml(m.text || "", m.viewing));
        else {
          const row = add("assistant", md(m.text || ""), m.steps);
          if (m.followups) renderFollowups(row, m.followups);
        }
      }
      if (!conv.messages.length) showEmpty();
      refreshConversations();
    } catch (e) { toast(errText(e), true); }
  }

  function newChat() {
    if (busy) return;
    conversationId = null;
    showEmpty();
    refreshConversations();
    input.focus();
  }
  $("conv-select").addEventListener("change", (e) => (e.target.value ? openConversation(e.target.value) : newChat()));
  $("new-chat").addEventListener("click", newChat);
  $("delete-chat").addEventListener("click", async () => {
    const c = conversations.find((x) => x.id === conversationId);
    if (!c || !confirm(`Delete the conversation "${c.title}"?\nThis removes the chat history only — not any emails or drafts.`)) return;
    await invoke("delete_conversation", { id: c.id }).catch((e) => toast(errText(e), true));
    newChat();
  });

  // ------------------------------------------------------------------ status
  function updatePrivacy() {
    const p = $("privacy");
    if (!p || !llmInfo) return;
    p.textContent = llmInfo.local
      ? "Reads Outlook on this PC with your local model. Replies go to Drafts and are never sent."
      : `Emails the assistant reads are sent to ${llmInfo.label}. Replies go to Drafts and are never sent.`;
  }

  async function health() {
    try {
      const h = await invoke("health");
      const set = (id, ok, title) => { const el = $(id); el.querySelector(".dot").className = "dot " + (ok ? "ok" : "bad"); el.title = title; };
      set("st-outlook", h.outlook.ok, h.outlook.ok ? "Connected" : h.outlook.error);
      const l = h.llm, el = $("st-llm");
      llmInfo = l;
      set("st-llm", l.ok && l.model_installed, !l.ok ? l.error : (l.model_installed ? `${l.label} · ${l.model}` : `${l.label}: model ${l.model} not offered`));
      el.querySelector(".lbl").textContent = `${l.label} · ${l.model}`;
      el.querySelector(".cloud")?.remove();
      if (!l.local) el.insertAdjacentHTML("beforeend", '<span class="cloud" title="Mail content is sent to this cloud service">cloud</span>');
      updatePrivacy();
      const w = $("storage-warning");
      w.hidden = !h.storage_warning; w.textContent = h.storage_warning || "";
    } catch (_) { /* backend busy */ }
  }

  // ------------------------------------------------------------------ settings panel
  const dlg = $("settings");
  let saved = null, draft = null, currentProv = "ollama";

  /// The chosen model: from the dropdown when the server's list is shown, else the text box.
  const currentModel = () => {
    const sel = $("f-model-select");
    return !sel.hidden && sel.value && sel.value !== "__other__" ? sel.value : $("f-model").value.trim();
  };
  const providerPatch = () => ({
    url: $("f-url").value.trim(), model: currentModel(),
    use_proxy: $("f-proxy").checked, proxy_url: $("f-proxy-url").value.trim(),
    ...($("f-key").value.trim() ? { api_key: $("f-key").value.trim() } : {}),
  });
  function stash() {
    const p = providerPatch();
    const d = { ...draft[currentProv], ...p, _key_typed: $("f-key").value };
    if (p.api_key) delete d.clear_api_key; else delete d.api_key;
    draft[currentProv] = d;
  }
  function showProvider(name) {
    currentProv = name;
    const meta = saved.providers[name], d = draft[name];
    $("f-url").value = d.url; $("f-model").value = d.model;
    $("f-model-select").hidden = true; $("f-model").hidden = false; // until this provider's list loads
    $("f-proxy").checked = d.use_proxy; $("f-proxy-url").value = d.proxy_url || "";
    $("f-proxy-url").disabled = !d.use_proxy;
    $("f-key").value = d._key_typed || "";
    const hasKey = meta.has_api_key && !d.clear_api_key;
    $("row-key").hidden = !meta.needs_key;
    $("f-key").placeholder = hasKey ? `Saved (${meta.api_key_hint}) — type to replace` : "Paste your API key";
    $("key-clear").hidden = !hasKey;
    $("key-help").textContent = ({
      openai: "From the OpenAI platform (platform.openai.com/api-keys).",
      anthropic: "From the Claude Console (platform.claude.com → API keys).",
      gemini: "From Google AI Studio (aistudio.google.com/apikey).",
      kimi: "From the Moonshot platform (platform.moonshot.ai).",
    })[name] || "";
    $("proxy-help").textContent = meta.local ? "Leave off for a local or intranet Ollama server." : "Usually needed on the corporate network to reach the internet.";
    $("ollama-only").hidden = name !== "ollama";
    $("cloud-warn").hidden = meta.local;
    document.querySelectorAll(".cloud-name").forEach((n) => { n.textContent = meta.label; });
    $("test-result").textContent = ""; $("test-result").className = "test-result";
    loadModels();
  }

  // Fill the model dropdown from the provider's server (Ollama /api/tags, Gemini/Kimi /models).
  let modelsToken = 0;
  async function loadModels() {
    const token = ++modelsToken, sel = $("f-model-select"), input = $("f-model"), out = $("test-result");
    const want = currentModel() || draft[currentProv].model;
    out.textContent = "Loading the model list from the server…"; out.className = "test-result";
    try {
      const h = await invoke("test_settings", { patch: fullPatch() });
      if (token !== modelsToken) return;
      if (!h.ok || !h.models.length) throw new Error(h.ok ? "The server lists no models." : h.error);
      const models = [...h.models].sort((a, b) => a.localeCompare(b));
      sel.replaceChildren(...models.map((m) => new Option((h.model_labels && h.model_labels[m]) || m, m)));
      if (want && !models.includes(want)) sel.prepend(new Option(`${want} (not on this server)`, want));
      sel.append(new Option("Other… (type a name)", "__other__"));
      sel.value = want || models[0];
      sel.hidden = false; input.hidden = true; input.value = sel.value;
      out.className = "test-result " + (models.includes(sel.value) ? "ok" : "bad");
      out.textContent = models.includes(sel.value) ? `Connected · ${models.length} models on the server.`
        : `Connected, but "${sel.value}" isn't on this server. Pick one from the list.`;
    } catch (e) {
      if (token !== modelsToken) return;
      sel.hidden = true; input.hidden = false; input.value = want || "";
      out.textContent = `Couldn't load the model list (${errText(e)}). Type the model name.`; out.className = "test-result bad";
    }
  }
  $("f-model-select").addEventListener("change", (e) => {
    const other = e.target.value === "__other__";
    $("f-model").hidden = !other;
    if (other) { $("f-model").value = ""; $("f-model").focus(); } else { $("f-model").value = e.target.value; }
  });
  // Reload the list when anything that decides which server we talk to changes.
  ["f-url", "f-proxy-url", "f-key"].forEach((id) => $(id).addEventListener("change", () => { stash(); loadModels(); }));
  $("f-proxy").addEventListener("change", () => { stash(); loadModels(); });
  function render() {
    const order = saved.order || Object.keys(saved.providers);
    $("providers").replaceChildren(...order.map((name) => [name, saved.providers[name]]).map(([name, p]) => {
      const lab = document.createElement("label");
      lab.innerHTML = `<input type="radio" name="provider" value="${esc(name)}"> ${esc(p.label)}<small>${p.local ? "local / intranet" : "cloud"}</small>`;
      lab.querySelector("input").checked = name === currentProv;
      lab.querySelector("input").addEventListener("change", () => { stash(); showProvider(name); });
      return lab;
    }));
    $("f-ctx").value = saved.num_ctx; $("f-think").value = saved.think || "";
    $("f-temp").value = saved.temperature; $("f-user").value = saved.user_name || "";
    $("config-file").textContent = saved.config_file; $("config-file").title = saved.config_file;
    showProvider(currentProv);
    if (saved.load_error) { $("test-result").textContent = saved.load_error; $("test-result").className = "test-result bad"; }
  }
  function fullPatch() {
    stash();
    const providers = {};
    for (const [name, d] of Object.entries(draft)) { const { _key_typed, ...rest } = d; providers[name] = rest; }
    return { provider: currentProv, providers, temperature: Number($("f-temp").value || 0.2),
             num_ctx: Number($("f-ctx").value || 16384), think: $("f-think").value, user_name: $("f-user").value.trim() };
  }
  async function openSettings() {
    try { saved = await invoke("get_settings"); } catch (e) { toast("Could not load settings: " + errText(e), true); return; }
    currentProv = saved.provider;
    draft = Object.fromEntries(Object.entries(saved.providers).map(([n, p]) => [n, { url: p.url, model: p.model, use_proxy: p.use_proxy, proxy_url: p.proxy_url || "" }]));
    render();
    dlg.showModal();
  }
  $("open-settings").addEventListener("click", openSettings);
  $("close-settings").addEventListener("click", () => dlg.close());
  $("cancel-settings").addEventListener("click", () => dlg.close());
  $("f-proxy").addEventListener("change", () => { $("f-proxy-url").disabled = !$("f-proxy").checked; });
  $("url-default").addEventListener("click", () => { $("f-url").value = saved.providers[currentProv].default_url; stash(); loadModels(); });
  $("key-clear").addEventListener("click", () => {
    draft[currentProv].clear_api_key = true; $("f-key").value = ""; stash(); showProvider(currentProv);
    $("test-result").textContent = "The key will be deleted when you save."; $("test-result").className = "test-result";
  });
  $("test").addEventListener("click", () => { stash(); loadModels(); });
  $("settings-form").addEventListener("submit", async (e) => {
    e.preventDefault();
    const b = $("save-settings"); b.disabled = true;
    try { saved = await invoke("save_settings", { patch: fullPatch() }); dlg.close(); health(); }
    catch (err) { $("test-result").textContent = errText(err); $("test-result").className = "test-result bad"; }
    finally { b.disabled = false; }
  });

  // ------------------------------------------------------------------ settings files (import / export)
  /// Pick a settings file and apply it. Returns true when something was imported.
  async function importSettings() {
    let r;
    try { r = await invoke("import_settings"); } catch (e) { toast(errText(e), true); return false; }
    if (!r) return false; // cancelled
    const parts = [r.llm && "LLM settings", r.profile && "profile"].filter(Boolean).join(" and ");
    const keys = r.api_keys ? ` (${r.api_keys} API key${r.api_keys > 1 ? "s" : ""})` : "";
    toast(`Imported ${parts}${keys}.${(r.notes || []).length ? " " + r.notes.join(" ") : ""}`);
    try { profileCache = await invoke("get_profile"); } catch (_) { /* ignore */ }
    if (dlg.open) { dlg.close(); openSettings(); }
    if (pdlg.open) fillProfile(profileCache);
    health();
    if (currentFolder) loadList(false);
    return true;
  }
  $("import-settings").addEventListener("click", importSettings);
  $("export-keys").addEventListener("change", () => {
    $("export-keys-help").classList.toggle("warn-text", $("export-keys").checked);
  });
  $("export-settings").addEventListener("click", async () => {
    const withKeys = $("export-keys").checked;
    if (withKeys && !confirm("The file will contain your API keys in plain text. Anyone with the file can use them. Continue?")) return;
    try {
      const path = await invoke("export_settings", { includeKeys: withKeys });
      if (path) toast(`Settings saved to ${path}` + (withKeys ? " (with API keys)." : " (without API keys)."));
    } catch (e) { toast(errText(e), true); }
  });

  // ------------------------------------------------------------------ profile dialog
  const pdlg = $("profile");
  const lines = (id) => $(id).value.split("\n").map((s) => s.trim()).filter(Boolean);
  const setLines = (id, arr) => { $(id).value = (arr || []).join("\n"); };
  let learnPoll = null;

  function lam(name) {
    name = name.trim();
    if (name.startsWith("ال")) return "لل" + name.slice(2);
    return /^[A-Za-z]/.test(name) ? "لـ " + name : "ل" + name;
  }
  function stylePreview() {
    const single = $("ps-single").value, font = $("ps-font").value.replace(/['";<>]/g, "");
    const arabic = /[؀-ۿ]/.test(single + $("ps-signoff").value) || $("ps-rtl").checked;
    const who = arabic ? "الأستاذ خالد" : "Sara";
    const greet = single.includes("ل{name}") ? single.replace("ل{name}", lam(who)) : single.replace("{name}", who);
    const body = arabic ? "نفيدكم بأنه تمت مراجعة الطلب، وسيتم الرد خلال يومين." : "Thank you, we have reviewed the request and will reply within two days.";
    const pv = $("ps-preview");
    pv.textContent = `${greet}\n\n${body}\n\n${$("ps-signoff").value}`;
    pv.style.fontFamily = `'${font}', Arial`; pv.style.fontSize = ($("ps-size").value || 14) + "pt";
    pv.dir = $("ps-rtl").checked ? "rtl" : "auto"; pv.style.textAlign = $("ps-rtl").checked ? "right" : "";
    pv.style.opacity = $("ps-enabled").checked ? 1 : 0.45;
  }
  ["ps-single", "ps-signoff", "ps-font", "ps-size", "ps-rtl", "ps-enabled"].forEach((id) => $(id).addEventListener("input", stylePreview));

  function fillProfile(pub) {
    const p = pub.profile;
    setLines("pf-names", p.my_names);
    $("pf-detected").textContent = pub.detected_name ? `Outlook says you are ${pub.detected_name}${pub.detected_email ? " <" + pub.detected_email + ">" : ""} — always included.` : "Outlook not connected: your own name could not be detected yet.";
    setLines("pf-mentions", p.my_mentions);
    $("pf-title").value = p.job_title || ""; $("pf-dept").value = p.dept_name || "";
    setLines("pf-keywords", p.dept_keywords); setLines("pf-dls", p.dept_addresses);
    $("pf-bg").value = p.background || "";
    $("pf-lang").value = p.language || "";
    const s = p.style;
    $("ps-enabled").checked = s.enabled; $("ps-single").value = s.greeting_single; $("ps-multi").value = s.greeting_multi;
    $("ps-special").value = (s.special || []).map((x) => `${x.matches} = ${x.greeting}`).join("\n");
    $("ps-signoff").value = s.signoff; $("ps-font").value = s.font_family; $("ps-size").value = s.font_size_pt; $("ps-rtl").checked = s.rtl;
    $("pl-enabled").checked = p.learning.enabled; $("pl-auto").checked = p.learning.auto_suggest; $("pl-days").value = p.learning.days_back;
    $("pf-markread").checked = p.mark_read !== false;
    stylePreview();
  }

  function collectProfile() {
    const base = profileCache.profile;
    return {
      ...base,
      my_names: lines("pf-names"), my_mentions: lines("pf-mentions"), job_title: $("pf-title").value.trim(),
      dept_name: $("pf-dept").value.trim(), dept_keywords: lines("pf-keywords"), dept_addresses: lines("pf-dls"),
      background: $("pf-bg").value,
      language: $("pf-lang").value.trim(),
      style: {
        enabled: $("ps-enabled").checked, greeting_single: $("ps-single").value.trim(), greeting_multi: $("ps-multi").value.trim(),
        special: lines("ps-special").map((l) => { const i = l.indexOf("="); return i < 0 ? null : { matches: l.slice(0, i).trim(), greeting: l.slice(i + 1).trim() }; }).filter(Boolean),
        signoff: $("ps-signoff").value.trim(), font_family: $("ps-font").value.trim(), font_size_pt: Number($("ps-size").value || 14), rtl: $("ps-rtl").checked,
      },
      learning: { ...base.learning, enabled: $("pl-enabled").checked, auto_suggest: $("pl-auto").checked, days_back: Number($("pl-days").value || 365) },
      mark_read: $("pf-markread").checked,
    };
  }

  async function refreshLearn() {
    try {
      const s = await invoke("learning_status");
      const last = s.last ? (s.last.error ? ` · last run ${s.last.at}: ${s.last.error}` : ` · last run ${s.last.at}`) : "";
      $("pl-status").textContent = s.running
        ? `Learning… ${s.scanned} sent items read so far. ${s.examples} replies learned, ${s.contacts} correspondents.`
        : `Learned from ${s.examples} of your replies · knows ${s.contacts} correspondents${last}.`;
      clearTimeout(learnPoll);
      if (pdlg.open) learnPoll = setTimeout(refreshLearn, s.running ? 1500 : 5000);
    } catch (e) { $("pl-status").textContent = errText(e); }
  }

  document.querySelectorAll(".tab").forEach((t) => t.addEventListener("click", () => {
    document.querySelectorAll(".tab").forEach((x) => x.classList.toggle("active", x === t));
    document.querySelectorAll(".tabpane").forEach((p) => { p.hidden = p.id !== t.dataset.tab; });
  }));
  $("open-profile").addEventListener("click", async () => {
    try { profileCache = await invoke("get_profile"); } catch (e) { toast(errText(e), true); return; }
    fillProfile(profileCache); $("profile-msg").textContent = "";
    pdlg.showModal(); refreshLearn();
  });
  $("close-profile").addEventListener("click", () => pdlg.close());
  $("cancel-profile").addEventListener("click", () => pdlg.close());

  $("pl-now").addEventListener("click", async () => { try { await invoke("learn_now"); setTimeout(refreshLearn, 500); } catch (e) { toast(errText(e), true); } });
  $("pl-forget").addEventListener("click", async () => {
    if (!confirm("Delete the replies learned from Sent Items and the saved suggestions? Your emails are not affected.")) return;
    try { await invoke("forget_learning"); refreshLearn(); } catch (e) { toast(errText(e), true); }
  });
  $("profile-form").addEventListener("submit", async (e) => {
    e.preventDefault();
    try {
      profileCache = await invoke("save_profile", { profile: collectProfile() });
      pdlg.close(); toast("Profile saved.");
      if (currentFolder) loadList(false);
    } catch (err) { $("profile-msg").textContent = errText(err); $("profile-msg").style.color = "var(--bad)"; }
  });

  // ------------------------------------------------------------------ first start of a new installation
  const wdlg = $("welcome");
  const welcomeDone = () => { invoke("welcome_done").catch(() => {}); wdlg.close(); };
  $("welcome-later").addEventListener("click", welcomeDone);
  $("welcome-setup").addEventListener("click", () => { welcomeDone(); openSettings(); });
  $("welcome-import").addEventListener("click", async () => {
    if (await importSettings()) wdlg.close();
  });
  wdlg.addEventListener("cancel", () => invoke("welcome_done").catch(() => {}));

  // ------------------------------------------------------------------ help (ui/help/*.html English, ui/help/ar/*.html Arabic)
  const hdlg = $("help");
  const hframe = $("help-frame");
  let helpInfo = null; // app_info: data_file, demo
  /// The language last read in Help; before that, Arabic if the Profile language is Arabic.
  function helpLang() {
    const saved = store.get("helpLang", "");
    if (saved === "ar" || saved === "en") return saved;
    const lang = (profileCache && profileCache.profile && profileCache.profile.language) || "";
    return /arab|عرب/i.test(lang) ? "ar" : "en";
  }
  function helpFooter(ar) {
    const file = helpInfo && helpInfo.data_file;
    $("help-data-label").textContent = ar ? "ملف بياناتك:" : "Your data file:";
    $("help-copy").textContent = ar ? "نسخ المسار" : "Copy path";
    $("help-copy").hidden = !file;
    $("help-data").classList.toggle("msg", !file);
    $("help-data").textContent = file || (helpInfo && helpInfo.demo
      ? (ar ? "لا يوجد — الوضع التجريبي لا يحفظ شيئاً" : "none — demo mode saves nothing")
      : (ar ? "لا يوجد — تعذّر فتح ملف التخزين (راجع الشريط الأصفر)" : "none — storage could not be opened (see the banner)"));
  }
  function openHelp(page) {
    if (page || !hframe.getAttribute("src")) hframe.setAttribute("src", `help/${helpLang() === "ar" ? "ar/" : ""}${page || "index.html"}`);
    if (!hdlg.open) hdlg.showModal();
  }
  $("open-help").addEventListener("click", () => openHelp());
  $("close-help").addEventListener("click", () => hdlg.close());
  document.addEventListener("keydown", (e) => {
    if (e.key === "F1") { e.preventDefault(); if (!document.querySelector("dialog[open]")) openHelp(); }
  });
  // The help pages have no scripts: web links open in the browser, Esc closes, F1 does nothing inside.
  hframe.addEventListener("load", () => {
    let doc;
    try { doc = hframe.contentDocument; } catch (_) { return; }
    if (!doc) return;
    const ar = (doc.documentElement.lang || "").startsWith("ar");
    store.set("helpLang", ar ? "ar" : "en");
    helpFooter(ar);
    doc.addEventListener("click", (e) => {
      const a = e.target.closest && e.target.closest("a[href]");
      if (a && /^https?:/i.test(a.getAttribute("href"))) {
        e.preventDefault();
        invoke("open_url", { url: a.href }).catch((er) => toast(errText(er), true));
      }
    });
    doc.addEventListener("keydown", (e) => {
      if (e.key === "Escape") hdlg.close();
      if (e.key === "F1") e.preventDefault();
    });
  });
  $("help-copy").addEventListener("click", () => {
    const path = $("help-data").textContent;
    const ar = helpLang() === "ar";
    navigator.clipboard.writeText(path).then(() => toast(ar ? "تم نسخ المسار." : "Path copied."))
      .catch(() => toast(ar ? "حدّد المسار واضغط Ctrl+C." : "Select the path and press Ctrl+C.", true));
  });

  // ------------------------------------------------------------------ start
  invoke("get_profile").then((p) => { profileCache = p; }).catch(() => {});
  invoke("app_info").then((i) => {
    document.title = `Inbox Chat ${i.version}${i.demo ? " — demo" : ""}`;
    $("help-version").textContent = `Inbox Chat ${i.version}`;
    helpInfo = i; helpFooter(helpLang() === "ar");
    if (i.first_run) wdlg.showModal();
  }).catch(() => {});
  showEmpty();
  refreshConversations();
  health();
  loadFolders(true);
})();
