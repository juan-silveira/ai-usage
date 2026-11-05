const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

let latest = [];
let sessions = [];
// "all" or a provider id; remembered per viewer.
let activeTab = "all";
try {
  activeTab = localStorage.getItem("activeTab") || "all";
} catch {}
// Sessions clicked while waiting: reminders stopped (key -> updated_at).
const acknowledged = new Map();

const STATUS_TEXT = {
  no_credentials: "Sem login — instale e entre na CLI deste provedor.",
  token_expired: "Token expirado — rode a CLI do provedor (claude, codex) para renovar.",
};

function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
}

function resetText(iso) {
  if (!iso) return "";
  const at = new Date(iso);
  const mins = Math.max(0, Math.round((at - Date.now()) / 60000));
  const d = Math.floor(mins / 1440), h = Math.floor((mins % 1440) / 60), m = mins % 60;
  const left = d > 0 ? `${d}d ${h}h` : `${h}h ${String(m).padStart(2, "0")}m`;
  const when = at.toLocaleString(undefined, { weekday: "short", hour: "2-digit", minute: "2-digit" });
  return `reinicia em ${left} · ${when}`;
}

function severity(pct) {
  return pct >= 90 ? "danger" : pct >= 70 ? "warn" : "";
}

function renderProvider(p) {
  const plan = p.plan ? `<span class="plan">${escapeHtml(p.plan)}</span>` : "";
  let body;
  if (p.status === "ok") {
    body = p.windows
      .map(
        (w) => `
        <div class="window">
          <div class="row"><span>${escapeHtml(w.label)}</span><strong>${w.used_percent.toFixed(0)}%</strong></div>
          <div class="bar"><div class="fill ${severity(w.used_percent)}" style="width:${w.used_percent}%"></div></div>
          <div class="muted">${resetText(w.resets_at)}</div>
        </div>`
      )
      .join("");
  } else {
    body = `<p class="status">${escapeHtml(STATUS_TEXT[p.status] ?? `Erro: ${p.message}`)}</p>`;
  }
  return `<section class="card"><h2>${escapeHtml(p.display_name)} ${plan}</h2>${body}</section>`;
}

function renderLimits() {
  const root = document.getElementById("providers");
  if (latest.length === 0) return;
  const shown = latest.filter((p) => activeTab === "all" || p.provider === activeTab);
  root.innerHTML = shown.length
    ? shown.map(renderProvider).join("")
    : `<p class="muted">Sem dados de limite para este provedor.</p>`;
  const at = new Date(latest[0].fetched_at);
  document.getElementById("updated").textContent =
    `Atualizado às ${at.toLocaleTimeString()} · a cada 60 s`;
}

function peakPercent(p) {
  return p && p.status === "ok" && p.windows.length
    ? Math.max(...p.windows.map((w) => w.used_percent))
    : null;
}

/** Providers known from either the usage poll or live sessions, in poll order. */
function providerIds() {
  const ids = latest.map((p) => p.provider);
  for (const s of sessions) if (!ids.includes(s.provider)) ids.push(s.provider);
  return ids;
}

function renderTabs() {
  const ids = providerIds();
  if (activeTab !== "all" && !ids.includes(activeTab) && latest.length) activeTab = "all";
  const tab = (id, label, extra) => `
    <button role="tab" class="tab" data-tab="${escapeHtml(id)}"
      aria-selected="${id === activeTab}" tabindex="${id === activeTab ? 0 : -1}">
      ${escapeHtml(label)}${extra}
    </button>`;
  const waiting = (id) =>
    sessions.filter((s) => s.state === "needs_input" && (id === "all" || s.provider === id)).length;
  const alertBadge = (n) => (n ? `<span class="count" title="${n} esperando você">${n}</span>` : "");

  document.getElementById("tabs").innerHTML =
    tab("all", "Todos", alertBadge(waiting("all"))) +
    ids
      .map((id) => {
        const usage = latest.find((p) => p.provider === id);
        const pct = peakPercent(usage);
        const pctBadge = pct == null ? "" : `<span class="pct ${severity(pct)}">${pct.toFixed(0)}%</span>`;
        return tab(id, usage?.display_name ?? PROVIDER_NAME[id] ?? id, pctBadge + alertBadge(waiting(id)));
      })
      .join("");
}

function render() {
  renderTabs();
  renderSessions();
  renderLimits();
}

function selectTab(id) {
  activeTab = id;
  try {
    localStorage.setItem("activeTab", id);
  } catch {}
  render();
  document.querySelector(`.tab[data-tab="${CSS.escape(id)}"]`)?.focus();
}

document.getElementById("tabs").addEventListener("click", (e) => {
  const tab = e.target.closest(".tab");
  if (tab) selectTab(tab.dataset.tab);
});

document.getElementById("tabs").addEventListener("keydown", (e) => {
  if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
  const ids = ["all", ...providerIds()];
  const i = ids.indexOf(activeTab);
  selectTab(ids[(i + (e.key === "ArrowRight" ? 1 : ids.length - 1)) % ids.length]);
});

const SESSION_STATE = {
  needs_input: "precisa de você",
  done: "concluída",
  working: "trabalhando",
  idle: "ociosa",
};
const PROVIDER_NAME = { claude: "Claude", codex: "Codex" };

function ago(iso) {
  const secs = Math.max(0, Math.round((Date.now() - new Date(iso)) / 1000));
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)} min`;
  return `${Math.floor(secs / 3600)}h${String(Math.floor((secs % 3600) / 60)).padStart(2, "0")}`;
}

function renderSession(s) {
  const key = `${s.provider}-${s.session_id}`;
  const since = s.state === "working" && s.turn_started_at ? s.turn_started_at : s.updated_at;
  const detail = s.detail ?? (s.state === "done" && s.last_turn_secs != null
    ? `turno de ${Math.max(1, Math.round(s.last_turn_secs / 60))} min`
    : "");
  return `
    <div class="session ${s.state}${acknowledged.get(key) === s.updated_at ? " acknowledged" : ""}" data-key="${escapeHtml(key)}" title="${escapeHtml(s.cwd)}">
      <div class="main">
        <div class="top">
          <span class="project">${escapeHtml(s.project)}</span>
          <span class="badge"><span class="dot">●</span> ${SESSION_STATE[s.state]} · ${ago(since)}</span>
        </div>
        <div class="detail">${escapeHtml(PROVIDER_NAME[s.provider] ?? s.provider)}${detail ? " · " + escapeHtml(detail) : ""}</div>
      </div>
    </div>`;
}

function renderSessions() {
  const root = document.getElementById("sessions");
  const shown = sessions.filter((s) => activeTab === "all" || s.provider === activeTab);
  root.innerHTML = shown.length
    ? shown.map(renderSession).join("")
    : `<p class="muted">Nenhuma sessão ativa.</p>`;
}

document.getElementById("sessions").addEventListener("click", (e) => {
  const card = e.target.closest(".session.needs_input");
  if (card) {
    const key = card.dataset.key;
    const s = sessions.find((x) => `${x.provider}-${x.session_id}` === key);
    if (s) acknowledged.set(key, s.updated_at);
    invoke("acknowledge_session", { key });
    renderSessions();
  }
});

listen("sessions-updated", (event) => {
  sessions = event.payload;
  render();
});

listen("usage-updated", (event) => {
  latest = event.payload;
  render();
});

document.getElementById("refresh").addEventListener("click", () => invoke("refresh_now"));

// Keep countdowns and elapsed times ticking between updates.
setInterval(renderLimits, 30_000);
setInterval(renderSessions, 5_000);

invoke("get_sessions").then((list) => {
  sessions = list;
  render();
});

invoke("get_usage").then((usage) => {
  latest = usage;
  render();
});
