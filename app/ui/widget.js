const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { getCurrentWindow } = window.__TAURI__.window;

const ICON = { claude: "icons/claude.png", codex: "icons/codex.png" };
let usage = [];
let sessions = [];
let levels = { session: [80, 95], weekly: [80, 95] };

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

const SHORT = { session: "5h", weekly: "sem" };

function line(w) {
  const k = SHORT[w.key] ?? w.label.replace(/^Semanal · /, "").slice(0, 4);
  return `<div class="line">
    <span class="k">${escapeHtml(k)}</span>
    <span class="bar"><span class="fill ${severity(w.used_percent, w.key)}" style="display:block;width:${w.used_percent}%"></span></span>
    <span class="pct">${w.used_percent.toFixed(0)}%</span>
    <span class="when">${left(w.resets_at)}</span>
  </div>`;
}

function render() {
  const ok = usage.filter((p) => p.status === "ok");
  document.getElementById("rows").innerHTML = ok.length
    ? ok
        .map((p) => {
          // Session and weekly only: per-model weekly caps would crowd it.
          const windows = p.windows.filter((w) => w.key === "session" || w.key === "weekly");
          const icon = ICON[p.provider]
            ? `<img src="${ICON[p.provider]}" alt="${escapeHtml(p.display_name)}" />`
            : `<span>${escapeHtml(p.display_name.slice(0, 2))}</span>`;
          return `<div class="row">${icon}<div>${windows.map(line).join("")}</div></div>`;
        })
        .join("")
    : `<div class="empty">Sem dados de uso</div>`;

  const waiting = sessions.filter((s) => s.state === "needs_input").length;
  const working = sessions.filter((s) => s.state === "working").length;
  const parts = [];
  if (waiting) parts.push(`<span class="waiting">⚠ ${waiting} esperando você</span>`);
  if (working) parts.push(`<span>● ${working} trabalhando</span>`);
  document.getElementById("sessions").innerHTML = parts.join("");
  fit();
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
  document.documentElement.style.setProperty("--bg-alpha", s.widget.opacity);
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

setInterval(render, 30_000);

Promise.all([invoke("get_usage"), invoke("get_sessions"), invoke("get_settings")]).then(([u, s, settings]) => {
  usage = u;
  sessions = s;
  applySettings(settings);
});
