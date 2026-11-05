const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { getCurrentWindow } = window.__TAURI__.window;

const ICON = { claude: "icons/claude.png", codex: "icons/codex.png", copilot: "icons/copilot.png", cursor: "icons/cursor.png", openrouter: "icons/openrouter.png" };
let usage = [];
let sessions = [];
let levels = { session: [80, 95], weekly: [80, 95] };
let sessionsMode = "summary";
let visible = ["claude", "codex"];
const shown = (list) => list.filter((x) => visible.includes(x.provider));

function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
}

function severity(pct, key) {
  const l = key === "session" ? levels.session : levels.weekly;
  if (pct >= 100) return "danger";
  return l.length && pct >= l[0] ? "warn" : "";
}

function left(iso) {
  if (!iso) return "";
  const secs = Math.max(0, (new Date(iso) - Date.now()) / 1000);
  if (secs < 3600) return `${Math.floor(secs / 60)}min`;
  if (secs < 86400) return `${Math.floor(secs / 3600)}h${String(Math.floor((secs % 3600) / 60)).padStart(2, "0")}`;
  return `${Math.floor(secs / 86400)}d${Math.floor((secs % 86400) / 3600)}h`;
}

function shortLabel(w) {
  if (w.key === "session") return t("short.session");
  if (w.key === "credits" || w.key === "key_limit") return "$";
  // Scoped windows ("Weekly · Opus", "Monthly · Chat"): the scope, short.
  if (w.key !== "weekly") return w.label.split("·").pop().trim().slice(0, 4);
  if (w.days == null || w.days === 7) return t("short.weekly");
  if (w.days >= 28 && w.days <= 31) return t("short.monthly");
  return t("short.days", { n: w.days });
}

function line(w) {
  const k = shortLabel(w);
  return `<div class="line">
    <span class="k">${escapeHtml(k)}</span>
    <span class="bar"><span class="fill ${severity(w.used_percent, w.key)}" style="display:block;width:${w.used_percent}%"></span></span>
    <span class="pct">${w.used_percent.toFixed(0)}%</span>
    <span class="when">${left(w.resets_at)}</span>
  </div>`;
}

function render() {
  const ok = shown(usage).filter((p) => p.status === "ok" && p.windows.length);
  document.getElementById("rows").innerHTML = ok.length
    ? ok
        .map((p) => {
          // Session and weekly only: per-model weekly caps would crowd it.
          // Providers with neither (Copilot's monthly quotas) show their first two.
          const main = p.windows.filter((w) => w.key === "session" || w.key === "weekly");
          const windows = main.length ? main : p.windows.slice(0, 2);
          const icon = ICON[p.provider]
            ? `<img src="${ICON[p.provider]}" alt="${escapeHtml(p.display_name)}" />`
            : `<span>${escapeHtml(p.display_name.slice(0, 2))}</span>`;
          return `<div class="row">${icon}<div>${windows.map(line).join("")}</div></div>`;
        })
        .join("")
    : `<div class="empty">${escapeHtml(t("widget.no_data"))}</div>`;

  document.getElementById("sessions").innerHTML =
    sessionsMode === "hidden" ? "" : sessionsMode === "list" ? sessionList() : sessionSummary();
  fit();
}

function sessionSummary() {
  const list = shown(sessions);
  const waiting = list.filter((s) => s.state === "needs_input").length;
  const working = list.filter((s) => s.state === "working").length;
  const parts = [];
  if (waiting) parts.push(`<span class="waiting">⚠ ${escapeHtml(t("tabs.waiting", { n: waiting }))}</span>`);
  if (working) parts.push(`<span>● ${escapeHtml(t("widget.working", { n: working }))}</span>`);
  return parts.join("");
}

const MARK = { needs_input: "⚠", working: "●", done: "✓", idle: "○" };
const MAX_LISTED = 6;

function ago(iso) {
  const secs = Math.max(0, Math.round((Date.now() - new Date(iso)) / 1000));
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}min`;
  return `${Math.floor(secs / 3600)}h${String(Math.floor((secs % 3600) / 60)).padStart(2, "0")}`;
}

// Sessions arrive sorted most urgent first.
function sessionList() {
  const list = shown(sessions);
  const rows = list.slice(0, MAX_LISTED).map((s) => {
    const mark = MARK[s.state] ?? "·";
    const label = escapeHtml(t(`session.state.${s.state}`));
    const since = s.state === "working" && s.turn_started_at ? s.turn_started_at : s.updated_at;
    const icon = ICON[s.provider] ? `<img src="${ICON[s.provider]}" alt="" />` : "";
    return `<div class="session ${s.state}">
      <span class="mark">${mark}</span>${icon}
      <span class="proj">${escapeHtml(s.project)}</span>
      <span class="st">${label}</span>
      <span class="when">${ago(since)}</span>
    </div>`;
  });
  const more = list.length - MAX_LISTED;
  if (more > 0) rows.push(`<div class="more">${escapeHtml(t("widget.more", { n: more }))}</div>`);
  return rows.join("");
}

// Shrink-wrap the native window around the content.
function fit() {
  requestAnimationFrame(() => {
    const r = document.getElementById("widget").getBoundingClientRect();
    invoke("widget_fit", { width: Math.ceil(r.width) + 2, height: Math.ceil(r.height) + 2 });
  });
}

function applySettings(s) {
  levels = { session: s.alert_session, weekly: s.alert_weekly };
  sessionsMode = s.widget.sessions;
  visible = s.widget.providers ?? visible;
  document.getElementById("sessions").classList.toggle("list", sessionsMode === "list");
  const root = document.documentElement;
  root.style.setProperty("--bg-alpha", s.widget.opacity);
  const custom = s.widget.color && /^#[0-9a-f]{6}$/i.test(s.widget.color) ? s.widget.color : null;
  if (custom) {
    const [r, g, b] = [1, 3, 5].map((i) => parseInt(custom.slice(i, i + 2), 16));
    root.style.setProperty("--bg-rgb", `${r} ${g} ${b}`);
    // Text follows the colour's brightness (relative luminance), not the theme.
    const lum = (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255;
    root.dataset.theme = lum > 0.55 ? "light" : "dark";
  } else {
    root.style.removeProperty("--bg-rgb");
    if (s.theme === "system") delete root.dataset.theme;
    else root.dataset.theme = s.theme;
  }
  render();
}

// Move mode: drag with the mouse, double click pins it again.
listen("widget-moving", (e) => {
  document.body.classList.toggle("moving", e.payload);
  fit();
});
document.addEventListener("mousedown", (e) => {
  if (document.body.classList.contains("moving") && e.button === 0 && e.detail === 1) {
    getCurrentWindow().startDragging();
  }
});
document.addEventListener("dblclick", () => {
  if (document.body.classList.contains("moving")) invoke("set_widget_moving", { moving: false });
});

listen("usage-updated", (e) => {
  usage = e.payload;
  render();
});
listen("sessions-updated", (e) => {
  sessions = e.payload;
  render();
});
listen("settings-updated", (e) => applySettings(e.payload));

// Often enough for the per-session elapsed times in list mode.
setInterval(render, 5_000);

Promise.all([invoke("get_usage"), invoke("get_sessions"), invoke("get_settings")]).then(([u, s, settings]) => {
  usage = u;
  sessions = s;
  applySettings(settings);
});
