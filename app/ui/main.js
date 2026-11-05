const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

let latest = [];
let sessions = [];
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

function render() {
  const root = document.getElementById("providers");
  if (latest.length === 0) return;
  root.innerHTML = latest.map(renderProvider).join("");
  const at = new Date(latest[0].fetched_at);
  document.getElementById("updated").textContent =
    `Atualizado às ${at.toLocaleTimeString()} · a cada 60 s`;
}

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
  root.innerHTML = sessions.length
    ? sessions.map(renderSession).join("")
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
  renderSessions();
});

listen("usage-updated", (event) => {
  latest = event.payload;
  render();
});

document.getElementById("refresh").addEventListener("click", () => invoke("refresh_now"));

// Keep countdowns and elapsed times ticking between updates.
setInterval(render, 30_000);
setInterval(renderSessions, 5_000);

invoke("get_sessions").then((list) => {
  sessions = list;
  renderSessions();
});

invoke("get_usage").then((usage) => {
  latest = usage;
  render();
});
