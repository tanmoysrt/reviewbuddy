/* reviewbuddy UI. Plain DOM on purpose: no build step, no CDN, works offline. */

const STATUS_LETTER = {
  added: "A", modified: "M", deleted: "D",
  renamed: "R", copied: "C", typechanged: "T", untracked: "U",
};

const state = {
  meta: null,
  tab: "changes",
  filter: "",
  view: store("view", "unified"),
  ctx: store("ctx", "3"),
  wrap: store("wrap", "0") === "1",
  tree: null,
  current: null, // { mode: "diff" | "file", path }
  listed: [],    // paths currently shown in the sidebar, in order
};

const $ = (sel) => document.querySelector(sel);
const els = {
  changedCount: $("#changed-count"),
  summary: $("#summary"),
  filter: $("#filter"),
  list: $("#file-list"),
  content: $("#content"),
  help: $("#help"),
};

/* ------------------------------------------------------------- utilities */

function store(key, fallback) {
  try {
    return localStorage.getItem("rb." + key) ?? fallback;
  } catch {
    return fallback;
  }
}

function remember(key, value) {
  try {
    localStorage.setItem("rb." + key, value);
  } catch {
    /* private browsing; the setting just will not persist */
  }
}

function esc(text) {
  return String(text ?? "").replace(/[&<>"]/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]
  );
}

/** Renders a path with the directory part dimmed. */
function pathHtml(path) {
  const cut = path.lastIndexOf("/");
  if (cut < 0) return esc(path);
  return `<span class="dir">${esc(path.slice(0, cut + 1))}</span>${esc(path.slice(cut + 1))}`;
}

function stat(additions, deletions) {
  const parts = [];
  if (additions) parts.push(`<span class="p">+${additions}</span>`);
  if (deletions) parts.push(`<span class="m">−${deletions}</span>`);
  return parts.join(" ");
}

async function api(endpoint, params) {
  const url = new URL(endpoint, location.origin);
  for (const [key, value] of Object.entries(params || {})) url.searchParams.set(key, value);
  const response = await fetch(url);
  const body = await response.json().catch(() => ({ error: response.statusText }));
  if (!response.ok) throw new Error(body.error || "request failed");
  return body;
}

function fail(message) {
  els.content.innerHTML = `<div class="error">${esc(message)}</div>`;
}

/* ------------------------------------------------------------------ boot */

async function boot() {
  applyTheme(store("theme", ""));
  els.content.classList.toggle("wrap", state.wrap);
  wireControls();
  if (await loadMeta()) route();
}

async function loadMeta() {
  try {
    state.meta = await api("/api/meta");
  } catch (error) {
    fail(`could not talk to reviewbuddy: ${error.message}`);
    return false;
  }
  const meta = state.meta;
  // The only place the comparison is stated; the page itself stays clean.
  const range = `${meta.base.label}${meta.merge_base ? "..." : ".."}${meta.head.label}`;
  document.title = `${meta.repo} · ${range}`;

  const additions = meta.files.reduce((sum, f) => sum + f.additions, 0);
  const deletions = meta.files.reduce((sum, f) => sum + f.deletions, 0);
  els.changedCount.textContent = meta.files.length;
  els.summary.innerHTML = `${meta.files.length} file${meta.files.length === 1 ? "" : "s"} &middot; ${stat(additions, deletions) || "no line changes"}`;
  renderSidebar();
  return true;
}

/* --------------------------------------------------------------- sidebar */

function renderSidebar() {
  if (state.tab === "changes") renderChanges();
  else renderBrowse();
}

function renderChanges() {
  const needle = state.filter.toLowerCase();
  const files = state.meta.files.filter((f) => f.path.toLowerCase().includes(needle));
  state.listed = files.map((f) => f.path);

  if (!files.length) {
    els.list.innerHTML = `<p class="note">${state.meta.files.length ? "No file matches the filter." : "No differences between these refs."}</p>`;
    return;
  }
  els.list.innerHTML = files.map((f) => itemHtml(f.path, "diff", f)).join("");
  markSelected();
}

async function renderBrowse() {
  if (!state.tree) {
    els.list.innerHTML = '<p class="note">Loading&hellip;</p>';
    try {
      state.tree = (await api("/api/tree")).files;
    } catch (error) {
      els.list.innerHTML = `<p class="note">${esc(error.message)}</p>`;
      return;
    }
  }

  if (state.filter) {
    const needle = state.filter.toLowerCase();
    const matches = state.tree.filter((p) => p.toLowerCase().includes(needle)).slice(0, 500);
    state.listed = matches;
    els.list.innerHTML = matches.length
      ? matches.map((p) => itemHtml(p, "file")).join("")
      : '<p class="note">Nothing matches.</p>';
    markSelected();
    return;
  }

  state.listed = state.tree;
  els.list.innerHTML = "";
  const tree = document.createElement("div");
  tree.className = "tree";
  tree.append(treeNodes(groupPaths(state.tree)));
  els.list.append(tree);
  markSelected();
}

function itemHtml(path, mode, entry) {
  const badge = entry
    ? `<span class="badge ${entry.status}" title="${entry.status}">${STATUS_LETTER[entry.status] || "?"}</span>`
    : "";
  const counts = entry ? `<span class="stat">${stat(entry.additions, entry.deletions)}</span>` : "";
  return `<button class="item" data-mode="${mode}" data-path="${esc(path)}" title="${esc(path)}">
    ${badge}<span class="name"><span>${pathHtml(path)}</span></span>${counts}
  </button>`;
}

/** Turns a flat path list into nested directory nodes. */
function groupPaths(paths) {
  const root = { dirs: new Map(), files: [] };
  for (const path of paths) {
    const parts = path.split("/");
    let node = root;
    for (const part of parts.slice(0, -1)) {
      if (!node.dirs.has(part)) node.dirs.set(part, { dirs: new Map(), files: [] });
      node = node.dirs.get(part);
    }
    node.files.push({ name: parts[parts.length - 1], path });
  }
  return root;
}

/** Builds one level of the tree; deeper levels fill in when opened. */
function treeNodes(node) {
  const frag = document.createDocumentFragment();
  for (const [name, child] of [...node.dirs].sort((a, b) => a[0].localeCompare(b[0]))) {
    const details = document.createElement("details");
    const summary = document.createElement("summary");
    summary.textContent = name;
    const children = document.createElement("div");
    children.className = "children";
    details.append(summary, children);
    details.addEventListener("toggle", () => {
      if (details.open && !children.firstChild) children.append(treeNodes(child));
    });
    frag.append(details);
  }
  for (const file of node.files) {
    const holder = document.createElement("div");
    holder.innerHTML = itemHtml(file.path, "file");
    const button = holder.firstElementChild;
    button.querySelector(".name").innerHTML = `<span>${esc(file.name)}</span>`;
    frag.append(button);
  }
  return frag;
}

function markSelected() {
  const path = state.current?.path;
  for (const item of els.list.querySelectorAll(".item")) {
    const selected = item.dataset.path === path;
    item.classList.toggle("on", selected);
    if (selected) item.scrollIntoView({ block: "nearest" });
  }
}

/* --------------------------------------------------------------- routing */

function open(mode, path) {
  location.hash = `${mode === "diff" ? "d" : "f"}/${encodeURIComponent(path)}`;
}

function route() {
  const match = /^#(d|f)\/(.*)$/.exec(location.hash);
  if (!match) {
    state.current = null;
    markSelected();
    welcome();
    return;
  }
  state.current = { mode: match[1] === "d" ? "diff" : "file", path: decodeURIComponent(match[2]) };
  markSelected();
  load();
}

function welcome() {
  const count = state.meta?.files.length ?? 0;
  els.content.innerHTML = `<div class="placeholder">
    <div>${count ? "Pick a file to review." : "No differences between these refs."}</div>
    <div>Press <b>?</b> for shortcuts.</div>
  </div>`;
}

async function load() {
  const { mode, path } = state.current;
  els.content.classList.toggle("wrap", state.wrap);
  try {
    if (mode === "diff") renderDiff(await api("/api/diff", { path, ctx: state.ctx }));
    else renderFile(await api("/api/file", { path }));
  } catch (error) {
    fail(error.message);
  }
  els.content.scrollTop = 0;
}

/* --------------------------------------------------------------- content */

/** The controls that used to live in the top bar, rendered per file. */
function controls(mode) {
  const bits = [];
  if (mode === "diff") {
    bits.push(
      `<span class="segmented">
        <button data-view="unified" class="${state.view === "unified" ? "on" : ""}">Unified</button>
        <button data-view="split" class="${state.view === "split" ? "on" : ""}">Split</button>
      </span>`,
      `<label class="control">Context <select data-ctx>${[
        ["3", "3"], ["8", "8"], ["25", "25"], ["full", "All"],
      ]
        .map(([value, text]) => `<option value="${value}"${value === state.ctx ? " selected" : ""}>${text}</option>`)
        .join("")}</select></label>`
    );
  }
  bits.push(
    `<button class="icon${state.wrap ? " on" : ""}" data-wrap title="Wrap long lines (w)">&#8617;</button>`
  );
  return bits.join("");
}

function renderDiff(file) {
  const head = `<div class="file-head">
    <span class="badge ${file.status}" title="${file.status}">${STATUS_LETTER[file.status] || "?"}</span>
    <span class="path">${pathHtml(file.path)}</span>
    ${file.old_path ? `<span class="rename">renamed from ${esc(file.old_path)}</span>` : ""}
    <span class="meta">
      ${file.language ? `<span>${esc(file.language)}</span>` : ""}
      <span class="stat">${stat(file.additions, file.deletions)}</span>
      ${controls("diff")}
      <button class="icon" id="whole-file" title="View the whole file">&#9776;</button>
    </span>
  </div>`;

  let body;
  if (file.state === "binary") body = '<p class="note">Binary file, not shown.</p>';
  else if (file.state === "missing") body = '<p class="note">This path exists on neither side.</p>';
  else if (file.state === "identical" || !file.hunks.length) {
    body = '<p class="note">No line changes (the file was renamed, or only its mode changed).</p>';
  } else {
    const table = state.view === "split" ? splitTable(file.hunks) : unifiedTable(file.hunks);
    body = `<div class="diff-scroll">${table}</div>`;
  }

  els.content.innerHTML = head + body;
}

function renderFile(file) {
  const head = `<div class="file-head">
    <span class="path">${pathHtml(file.path)}</span>
    <span class="meta">
      ${file.language ? `<span>${esc(file.language)}</span>` : ""}
      <span>${file.lines.length} lines</span>
      ${controls("file")}
      ${inDiff(file.path) ? '<button class="icon" id="show-diff" title="Show the diff">&#8646;</button>' : ""}
    </span>
  </div>`;

  let body;
  if (file.state === "binary") body = '<p class="note">Binary file, not shown.</p>';
  else if (file.state === "missing") body = '<p class="note">No such file on this side.</p>';
  else {
    const rows = file.lines
      .map((html, i) => `<tr class="ctx"><td class="ln">${i + 1}</td><td class="code">${html}</td></tr>`)
      .join("");
    body = `<div class="diff-scroll"><table class="diff plain"><tbody>${rows}</tbody></table></div>`;
  }

  els.content.innerHTML = head + body;
}

function inDiff(path) {
  return !!state.meta?.files.some((f) => f.path === path);
}

function eof(row) {
  return row.no_newline ? '<span class="eof"> ↵ no newline at end of file</span>' : "";
}

function unifiedTable(hunks) {
  const out = ['<table class="diff unified"><tbody>'];
  for (const hunk of hunks) {
    out.push(`<tr class="hunk"><td colspan="3">${esc(hunk.header)}</td></tr>`);
    for (const row of hunk.rows) {
      out.push(
        `<tr class="${row.kind}"><td class="ln">${row.old ?? ""}</td><td class="ln">${row.new ?? ""}</td>` +
          `<td class="code">${row.html}${eof(row)}</td></tr>`
      );
    }
  }
  out.push("</tbody></table>");
  return out.join("");
}

function splitTable(hunks) {
  const out = ['<table class="diff split"><tbody>'];
  for (const hunk of hunks) {
    out.push(`<tr class="hunk"><td colspan="4">${esc(hunk.header)}</td></tr>`);
    for (const [left, right] of pairRows(hunk.rows)) {
      out.push(`<tr>${side(left, "del")}${side(right, "add")}</tr>`);
    }
  }
  out.push("</tbody></table>");
  return out.join("");
}

function side(row, kind) {
  if (!row) return '<td class="ln"></td><td class="code empty"></td>';
  const number = kind === "del" ? row.old : row.new;
  const cls = row.kind === "ctx" ? "" : " " + kind;
  return `<td class="ln">${number ?? ""}</td><td class="code${cls}">${row.html}${eof(row)}</td>`;
}

/** Lines up each removal with the insertion that replaced it. */
function pairRows(rows) {
  const pairs = [];
  let i = 0;
  while (i < rows.length) {
    if (rows[i].kind === "ctx") {
      pairs.push([rows[i], rows[i]]);
      i += 1;
      continue;
    }
    const removed = [];
    const added = [];
    while (i < rows.length && rows[i].kind === "del") removed.push(rows[i++]);
    while (i < rows.length && rows[i].kind === "add") added.push(rows[i++]);
    for (let j = 0; j < Math.max(removed.length, added.length); j++) {
      pairs.push([removed[j] || null, added[j] || null]);
    }
  }
  return pairs;
}

/* --------------------------------------------------------------- controls */

function setView(view) {
  state.view = view;
  remember("view", view);
  if (state.current?.mode === "diff") load();
}

function toggleWrap() {
  state.wrap = !state.wrap;
  remember("wrap", state.wrap ? "1" : "0");
  els.content.classList.toggle("wrap", state.wrap);
  for (const button of els.content.querySelectorAll("[data-wrap]")) {
    button.classList.toggle("on", state.wrap);
  }
}

function setTab(tab) {
  state.tab = tab;
  for (const button of document.querySelectorAll("[data-tab]")) {
    button.classList.toggle("on", button.dataset.tab === tab);
  }
  renderSidebar();
}

function applyTheme(theme) {
  if (theme) document.documentElement.dataset.theme = theme;
  else delete document.documentElement.dataset.theme;
  remember("theme", theme);
}

function toggleTheme() {
  const dark = getComputedStyle(document.documentElement).colorScheme.includes("dark");
  applyTheme(dark ? "light" : "dark");
}

function wireControls() {
  els.list.addEventListener("click", (event) => {
    const item = event.target.closest(".item");
    if (item) open(item.dataset.mode, item.dataset.path);
  });

  for (const button of document.querySelectorAll("[data-tab]")) {
    button.addEventListener("click", () => setTab(button.dataset.tab));
  }

  els.content.addEventListener("click", (event) => {
    const view = event.target.closest("[data-view]");
    if (view) return setView(view.dataset.view);
    if (event.target.closest("[data-wrap]")) return toggleWrap();
    if (event.target.closest("#whole-file")) return open("file", state.current.path);
    if (event.target.closest("#show-diff")) return open("diff", state.current.path);
  });

  els.content.addEventListener("change", (event) => {
    if (!event.target.matches("[data-ctx]")) return;
    state.ctx = event.target.value;
    remember("ctx", state.ctx);
    if (state.current?.mode === "diff") load();
  });

  $("#theme").addEventListener("click", toggleTheme);
  $("#reload").addEventListener("click", reload);
  $("#help-toggle").addEventListener("click", () => (els.help.hidden = !els.help.hidden));
  els.help.addEventListener("click", () => (els.help.hidden = true));

  els.filter.addEventListener("input", () => {
    state.filter = els.filter.value.trim();
    renderSidebar();
  });

  window.addEventListener("hashchange", route);
  document.addEventListener("keydown", onKey);
  wireResizer();
}

async function reload() {
  state.tree = null;
  await loadMeta();
  if (state.current) load();
}

function step(delta) {
  const index = state.listed.indexOf(state.current?.path);
  const next = state.listed[Math.max(0, Math.min(state.listed.length - 1, index + delta))];
  if (next) open(state.tab === "changes" ? "diff" : "file", next);
}

/** Scrolls to the next or previous hunk header in the current file. */
function hunk(delta) {
  const headers = [...els.content.querySelectorAll("tr.hunk")];
  if (!headers.length) return;
  const origin = els.content.getBoundingClientRect().top - els.content.scrollTop;
  const tops = headers.map((row) => row.getBoundingClientRect().top - origin);
  const here = els.content.scrollTop + 46;
  const next = tops.findIndex((top) => top > here + 1);
  const target = delta > 0
    ? (next === -1 ? tops[tops.length - 1] : tops[next])
    : tops[Math.max(0, (next === -1 ? tops.length : next) - 2)];
  els.content.scrollTo({ top: Math.max(0, target - 46), behavior: "smooth" });
}

function onKey(event) {
  if (event.metaKey || event.ctrlKey || event.altKey) return;
  if (event.target.matches("input, select, textarea")) {
    if (event.key === "Escape") {
      els.help.hidden = true;
      event.target.blur();
    } else if (event.target === els.filter) {
      if (event.key === "ArrowDown") step(1);
      else if (event.key === "ArrowUp") step(-1);
      else if (event.key === "Enter") {
        if (state.listed.includes(state.current?.path)) els.filter.blur();
        else step(1);
      } else return;
      event.preventDefault();
    }
    return;
  }

  const actions = {
    j: () => step(1),
    k: () => step(-1),
    n: () => hunk(1),
    p: () => hunk(-1),
    u: () => setView(state.view === "unified" ? "split" : "unified"),
    w: () => toggleWrap(),
    r: () => reload(),
    1: () => setTab("changes"),
    2: () => setTab("browse"),
    "?": () => (els.help.hidden = !els.help.hidden),
    Escape: () => (els.help.hidden = true),
    "/": () => els.filter.focus(),
  };
  const action = actions[event.key];
  if (action) {
    event.preventDefault();
    action();
  }
}

function wireResizer() {
  const resizer = $("#resizer");
  resizer.addEventListener("pointerdown", (event) => {
    resizer.setPointerCapture(event.pointerId);
    resizer.classList.add("active");
    const move = (e) => {
      const width = Math.min(640, Math.max(180, e.clientX));
      document.documentElement.style.setProperty("--sidebar", width + "px");
    };
    const stop = () => {
      resizer.classList.remove("active");
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", stop);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", stop);
  });
}

boot();
