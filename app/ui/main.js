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
  rate_limited: "Limite de consultas atingido (HTTP 429). Nova tentativa automática em alguns minutos.",
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

// Green below the window's first alert level, yellow from it, red only when
// the window is used up (100%).
let levels = { session: [80, 95], weekly: [80, 95] };
function severity(pct, key = "session") {
  const l = key === "session" ? levels.session : levels.weekly;
  if (pct >= 100) return "danger";
  return l.length && pct >= l[0] ? "warn" : "";
}
const RANK = { "": 0, warn: 1, danger: 2 };
function worstSeverity(p) {
  if (p?.status !== "ok") return "";
  return p.windows.map((w) => severity(w.used_percent, w.key)).sort((a, b) => RANK[b] - RANK[a])[0] ?? "";
}

function staleNotice(p) {
  if (!p.stale) return "";
  const at = new Date(p.fetched_at).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
  const retry = p.stale.retry_at
    ? ` · nova tentativa às ${new Date(p.stale.retry_at).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })}`
    : "";
  return `<p class="stale">Dados de ${at} — ${escapeHtml(p.stale.reason)}${retry}</p>`;
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
          <div class="bar"><div class="fill ${severity(w.used_percent, w.key)}" style="width:${w.used_percent}%"></div></div>
          <div class="muted">${resetText(w.resets_at)}</div>
        </div>`
      )
      .join("");
  } else {
    body = `<p class="status">${escapeHtml(STATUS_TEXT[p.status] ?? `Erro: ${p.message}`)}</p>`;
  }
  return `<section class="card"><h2>${escapeHtml(p.display_name)} ${plan}</h2>${body}${staleNotice(p)}</section>`;
}

function renderLimits() {
  const root = document.getElementById("providers");
  if (latest.length === 0) return;
  const shown = latest.filter((p) => activeTab === "all" || p.provider === activeTab);
  root.innerHTML = shown.length
    ? shown.map(renderProvider).join("")
    : `<p class="muted">Sem dados de limite para este provedor.</p>`;
  const fresh = latest.filter((p) => !p.stale).map((p) => new Date(p.fetched_at));
  const at = fresh.length ? new Date(Math.max(...fresh)) : new Date(latest[0].fetched_at);
  document.getElementById("updated").textContent =
    `Atualizado às ${at.toLocaleTimeString()} · a cada ${pollEvery} min`;
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
  if (activeTab !== "all" && activeTab !== "settings" && !ids.includes(activeTab) && latest.length) activeTab = "all";
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
        const pctBadge = pct == null ? "" : `<span class="pct ${worstSeverity(usage)}">${pct.toFixed(0)}%</span>`;
        return tab(id, usage?.display_name ?? PROVIDER_NAME[id] ?? id, pctBadge + alertBadge(waiting(id)));
      })
      .join("") +
    `<button role="tab" class="tab gear" data-tab="settings" title="Preferências" aria-label="Preferências"
      aria-selected="${activeTab === "settings"}" tabindex="${activeTab === "settings" ? 0 : -1}">⚙</button>`;
  document.getElementById("view-usage").hidden = activeTab === "settings";
  document.getElementById("view-settings").hidden = activeTab !== "settings";
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
  const ids = ["all", ...providerIds(), "settings"];
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
  // Don't rebuild the form under the user's cursor while they type a name.
  if (!document.getElementById("provider-bar").contains(document.activeElement)) renderProviderBar();
});

document.getElementById("refresh").addEventListener("click", () => invoke("refresh_now"));

// Keep countdowns and elapsed times ticking between updates.
setInterval(renderLimits, 30_000);
setInterval(renderSessions, 5_000);

const barFormat = document.getElementById("bar-format");
barFormat.addEventListener("change", () => invoke("set_bar_format", { format: barFormat.value }));

const soundToggles = {
  needs_input: document.getElementById("sound-needs-input"),
  done: document.getElementById("sound-done"),
  threshold: document.getElementById("sound-threshold"),
};
for (const [sound, box] of Object.entries(soundToggles)) {
  box.addEventListener("change", () => invoke("set_sound_enabled", { sound, enabled: box.checked }));
}
document.querySelectorAll("[data-preview]").forEach((btn) =>
  btn.addEventListener("click", () => invoke("preview_sound", { sound: btn.dataset.preview }))
);

const PROVIDER_ICON = { claude: "icons/claude.png", codex: "icons/codex.png" };
let barSettings = null;

/** Mirrors bar.rs so the preview matches what the panel will show. */
function barPreview(id, cfg) {
  const usage = latest.find((p) => p.provider === id);
  let value = "—";
  if (usage?.status === "ok") {
    const pct = (k) => usage.windows.find((w) => w.key === k)?.used_percent;
    const peak = Math.max(...usage.windows.map((w) => w.used_percent));
    const f = barSettings.bar_format;
    if (f === "session") value = `${Math.round(pct("session") ?? peak)}%`;
    else if (f === "session_weekly" && pct("session") != null && pct("weekly") != null)
      value = `${Math.round(pct("session"))}/${Math.round(pct("weekly"))}%`;
    else value = `${Math.round(peak)}%`;
  }
  if (usage?.status === "ok" && barSettings.bar_reset !== "off") {
    const f = barSettings.bar_format;
    const byPeak = [...usage.windows].sort((a, b) => b.used_percent - a.used_percent)[0];
    const w = ((f === "session" || f === "session_weekly") && usage.windows.find((x) => x.key === "session")) || byPeak;
    if (w?.resets_at) {
      const at = new Date(w.resets_at);
      const secs = Math.max(0, (at - Date.now()) / 1000);
      let when;
      if (barSettings.bar_reset === "remaining") {
        when = secs < 3600 ? `${Math.floor(secs / 60)}min`
          : secs < 86400 ? `${Math.floor(secs / 3600)}h${String(Math.floor((secs % 3600) / 60)).padStart(2, "0")}`
          : `${Math.floor(secs / 86400)}d${Math.floor((secs % 86400) / 3600)}h`;
      } else {
        const hm = at.toLocaleTimeString("pt-BR", { hour: "2-digit", minute: "2-digit" });
        when = secs < 86400 ? hm : `${["dom", "seg", "ter", "qua", "qui", "sex", "sáb"][at.getDay()]} ${hm}`;
      }
      value += ` · ${when}`;
    }
  }
  const icon = cfg.show_icon ? PROVIDER_ICON[id] : null;
  const name = cfg.name.trim() || barSettings.default_names[id];
  const text = cfg.show_name || (cfg.show_icon && !icon) ? `${name} ${value}` : value;
  return `<span class="preview">${icon ? `<img src="${icon}" alt="" />` : ""}${escapeHtml(text)}</span>`;
}

function renderProviderBar() {
  if (!barSettings) return;
  const off = barSettings.bar_format === "off";
  document.getElementById("provider-bar").innerHTML = Object.entries(barSettings.providers)
    .map(([id, cfg]) => {
      const usage = latest.find((p) => p.provider === id);
      const label = usage?.display_name ?? PROVIDER_NAME[id] ?? id;
      const icon = PROVIDER_ICON[id];
      const dim = off || !cfg.show ? " disabled" : "";
      return `
      <div class="pbar" data-provider="${escapeHtml(id)}">
        <div class="head">
          <label class="who">
            <input type="checkbox" data-field="show" ${cfg.show ? "checked" : ""} ${off ? "disabled" : ""} />
            ${icon ? `<img src="${icon}" alt="" />` : ""}${escapeHtml(label)}
          </label>
          ${off || !cfg.show ? "" : barPreview(id, cfg)}
        </div>
        <div class="opts${dim}">
          <label><input type="checkbox" data-field="show_icon" ${cfg.show_icon ? "checked" : ""} /> Ícone</label>
          <label><input type="checkbox" data-field="show_name" ${cfg.show_name ? "checked" : ""} /> Nome</label>
          <input type="text" data-field="name" maxlength="16" value="${escapeHtml(cfg.name)}"
            placeholder="${escapeHtml(barSettings.default_names[id] ?? "")}" ${cfg.show_name ? "" : "disabled"} />
        </div>
      </div>`;
    })
    .join("");
}

function saveProviderBar(id) {
  invoke("set_provider_bar", { provider: id, config: barSettings.providers[id] });
}

const providerBar = document.getElementById("provider-bar");
providerBar.addEventListener("change", (e) => {
  const row = e.target.closest(".pbar");
  const field = e.target.dataset.field;
  if (!row || !field) return;
  const cfg = barSettings.providers[row.dataset.provider];
  cfg[field] = e.target.type === "checkbox" ? e.target.checked : e.target.value;
  saveProviderBar(row.dataset.provider);
  renderProviderBar();
});
// Live preview while typing; saved on change (blur / Enter).
providerBar.addEventListener("input", (e) => {
  const row = e.target.closest(".pbar");
  if (!row || e.target.dataset.field !== "name") return;
  barSettings.providers[row.dataset.provider].name = e.target.value;
  const preview = row.querySelector(".preview");
  if (preview) preview.outerHTML = barPreview(row.dataset.provider, barSettings.providers[row.dataset.provider]);
});

barFormat.addEventListener("change", () => {
  if (barSettings) barSettings.bar_format = barFormat.value;
  renderProviderBar();
});

const barReset = document.getElementById("bar-reset");
barReset.addEventListener("change", () => {
  invoke("set_bar_reset", { reset: barReset.value });
  if (barSettings) barSettings.bar_reset = barReset.value;
  renderProviderBar();
});

const levelInputs = document.querySelectorAll(".levels");
levelInputs.forEach((input) =>
  input.addEventListener("change", async () => {
    const values = input.value.split(/[^0-9]+/).filter(Boolean).map(Number);
    const window = input.dataset.window;
    levels[window] = await invoke("set_alert_thresholds", { window, thresholds: values });
    input.value = levels[window].join(", ");
    render();
  })
);

// Updates: banner on the main view, status and manual check in settings.
const updateBanner = document.getElementById("update-banner");
const updateStatus = document.getElementById("update-status");
function showUpdate(status) {
  const u = status.update;
  updateBanner.hidden = !u;
  if (u) document.getElementById("update-text").textContent = `Nova versão ${u.version} disponível`;
  updateStatus.textContent = status.error
    ? `Versão ${status.current} · não foi possível verificar (${status.error})`
    : u
      ? `Versão ${status.current} · ${u.version} disponível`
      : `Versão ${status.current} · nenhuma atualização encontrada`;
}
document.getElementById("update-open").addEventListener("click", () => invoke("open_update"));
document.getElementById("check-now").addEventListener("click", async () => {
  updateStatus.textContent = "Verificando…";
  showUpdate(await invoke("check_updates_now"));
});
const pollMinutes = document.getElementById("poll-minutes");
let pollEvery = 5;
pollMinutes.addEventListener("change", () => {
  pollEvery = Number(pollMinutes.value);
  invoke("set_poll_minutes", { minutes: pollEvery });
  render();
});

const checkUpdates = document.getElementById("check-updates");
checkUpdates.addEventListener("change", () => invoke("set_check_updates", { enabled: checkUpdates.checked }));
listen("update-status", (e) => showUpdate({ current: "", update: e.payload, error: null }));
invoke("get_update").then(showUpdate);

const widgetEnabled = document.getElementById("widget-enabled");
const widgetMove = document.getElementById("widget-move");
const widgetOpacity = document.getElementById("widget-opacity");
let widgetMoving = false;

function renderWidgetControls() {
  widgetMove.disabled = !widgetEnabled.checked;
  widgetMove.textContent = widgetMoving ? "Fixar" : "Mover";
  widgetMove.classList.toggle("active", widgetMoving);
}
widgetEnabled.addEventListener("change", async () => {
  await invoke("set_widget_enabled", { enabled: widgetEnabled.checked });
  widgetMoving = false;
  renderWidgetControls();
});
widgetMove.addEventListener("click", () => invoke("set_widget_moving", { moving: !widgetMoving }));
widgetOpacity.addEventListener("input", () =>
  invoke("set_widget_opacity", { opacity: Number(widgetOpacity.value) / 100 })
);
listen("widget-moving", (e) => {
  widgetMoving = e.payload;
  renderWidgetControls();
});

invoke("get_settings").then((settings) => {
  widgetEnabled.checked = settings.widget.enabled;
  widgetOpacity.value = Math.round(settings.widget.opacity * 100);
  renderWidgetControls();
  barSettings = settings;
  barReset.value = settings.bar_reset;
  levels = { session: settings.alert_session, weekly: settings.alert_weekly };
  levelInputs.forEach((input) => (input.value = levels[input.dataset.window].join(", ")));
  checkUpdates.checked = settings.check_updates;
  pollEvery = settings.poll_minutes;
  pollMinutes.value = String(pollEvery);
  soundToggles.threshold.checked = settings.sound_threshold;
  render();
  renderProviderBar();
  barFormat.value = settings.bar_format;
  soundToggles.needs_input.checked = settings.sound_needs_input;
  soundToggles.done.checked = settings.sound_done;
});

invoke("get_sessions").then((list) => {
  sessions = list;
  render();
});

invoke("get_usage").then((usage) => {
  latest = usage;
  render();
  renderProviderBar();
});

// ---------- Theme ----------
const themeSelect = document.getElementById("theme");
function applyTheme(theme) {
  if (theme === "system") delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = theme;
}
themeSelect.addEventListener("change", () => {
  applyTheme(themeSelect.value);
  invoke("set_theme", { theme: themeSelect.value });
});
invoke("get_settings").then((s) => {
  themeSelect.value = s.theme;
  applyTheme(s.theme);
});

// ---------- Connections ----------
let connections = [];
const loginState = {}; // provider -> { url, line, result }

function connStatus(id) {
  const p = latest.find((x) => x.provider === id);
  if (!p) return { text: "Verificando…", cls: "" };
  switch (p.status) {
    case "ok":
      return { text: `Conectado${p.plan ? ` · plano ${p.plan}` : ""}`, cls: "ok" };
    case "token_expired":
      return { text: "Login expirado — reconecte", cls: "bad" };
    case "no_credentials":
      return { text: "Não conectado", cls: "bad" };
    case "rate_limited":
      return { text: "Conectado · consultas limitadas no momento", cls: "ok" };
    default:
      return { text: "Erro ao consultar", cls: "bad" };
  }
}

function renderConnections() {
  const root = document.getElementById("connections");
  if (!connections.length) return;
  if (root.contains(document.activeElement) && document.activeElement.tagName === "INPUT"
      && document.activeElement.type === "text") return; // don't wipe a code being typed
  root.innerHTML = connections
    .map((c) => {
      const st = connStatus(c.provider);
      const icon = PROVIDER_ICON[c.provider];
      const login = loginState[c.provider] ?? {};
      const connected = st.cls === "ok";
      let action;
      if (!c.cli_path) {
        action = `<span class="state bad">CLI não encontrada</span>`;
      } else if (c.login_running) {
        action = `<button class="text" data-act="cancel">Cancelar</button>`;
      } else {
        const expired = latest.find((x) => x.provider === c.provider)?.status === "token_expired";
        action = `<button class="text" data-act="login">${connected || expired ? "Reconectar" : "Conectar"}</button>`;
        if (connected || expired) action = `<div class="actions">${action}<button class="text" data-act="logout">Desconectar</button></div>`;
      }
      const loginBox = c.login_running
        ? `<div class="login">
            <span>Conclua o login no navegador que abriu.</span>
            ${login.url ? `<span>Se ele não abriu: <a href="#" data-act="open-url">abrir link de login</a></span>` : ""}
            <div class="row">
              <input type="text" data-field="code" placeholder="Código, se a CLI pedir" />
              <button class="text" data-act="send-code">Enviar</button>
            </div>
            ${login.line ? `<span class="line">${escapeHtml(login.line)}</span>` : ""}
          </div>`
        : login.result === false
          ? `<div class="login"><span>O login não foi concluído.</span>${login.line ? `<span class="line">${escapeHtml(login.line)}</span>` : ""}</div>`
          : "";
      const logoutBox = login.confirmLogout
        ? `<div class="login">
            <span>Isso roda <code>${escapeHtml(c.provider === "claude" ? "claude auth logout" : "codex logout")}</code>: o ${escapeHtml(c.name)} também sai da conta no terminal.</span>
            <div class="row">
              <button class="text" data-act="logout-confirm">Desconectar</button>
              <button class="text" data-act="logout-cancel">Cancelar</button>
            </div>
          </div>`
        : login.logoutError
          ? `<div class="login"><span>Não foi possível desconectar.</span><span class="line">${escapeHtml(login.logoutError)}</span></div>`
          : "";
      const install = c.cli_path
        ? ""
        : `<div class="row"><code>${escapeHtml(c.install_command)}</code>
             <button class="text" data-act="copy">Copiar</button></div>`;
      return `
      <div class="conn" data-provider="${escapeHtml(c.provider)}">
        <div class="head">
          <div class="who">
            ${icon ? `<img src="${icon}" alt="" />` : ""}
            <div><div class="name">${escapeHtml(c.name)}</div>
            <div class="state ${st.cls}">${escapeHtml(st.text)}</div></div>
          </div>
          ${action}
        </div>
        ${loginBox}${logoutBox}${install}
        <label class="pref"><span><input type="checkbox" data-act="hooks" ${c.hooks_installed ? "checked" : ""} ${c.cli_path ? "" : "disabled"} />
          Sessões ao vivo (alertas de “precisa de você” e “concluiu”)</span></label>
      </div>`;
    })
    .join("");
}

async function loadConnections() {
  connections = await invoke("get_connections");
  renderConnections();
}

document.getElementById("connections").addEventListener("click", async (e) => {
  const act = e.target.dataset.act;
  const row = e.target.closest(".conn");
  if (!act || !row || act === "hooks") return;
  const provider = row.dataset.provider;
  const conn = connections.find((c) => c.provider === provider);
  if (act === "login") {
    loginState[provider] = {};
    try {
      await invoke("start_login", { provider });
    } catch (err) {
      loginState[provider] = { result: false, line: String(err) };
    }
    await loadConnections();
  } else if (act === "logout") {
    loginState[provider] = { confirmLogout: true };
    renderConnections();
  } else if (act === "logout-cancel") {
    loginState[provider] = {};
    renderConnections();
  } else if (act === "logout-confirm") {
    e.target.disabled = true;
    try {
      await invoke("logout", { provider });
      loginState[provider] = {};
    } catch (err) {
      loginState[provider] = { logoutError: String(err) };
    }
    await loadConnections();
  } else if (act === "cancel") {
    await invoke("cancel_login", { provider });
    await loadConnections();
  } else if (act === "send-code") {
    const input = row.querySelector('[data-field="code"]');
    if (input.value.trim()) await invoke("send_login_code", { provider, code: input.value });
    input.value = "";
  } else if (act === "open-url") {
    e.preventDefault();
    invoke("open_url", { url: loginState[provider]?.url });
  } else if (act === "copy") {
    navigator.clipboard?.writeText(conn.install_command);
    e.target.textContent = "Copiado";
  }
});

document.getElementById("connections").addEventListener("change", async (e) => {
  if (e.target.dataset.act !== "hooks") return;
  const provider = e.target.closest(".conn").dataset.provider;
  try {
    await invoke("set_hooks", { provider, enabled: e.target.checked });
  } catch (err) {
    alert(`Não foi possível alterar os hooks: ${err}`);
  }
  await loadConnections();
});

listen("login-event", (e) => {
  const { provider, line, url, finished } = e.payload;
  const st = (loginState[provider] ??= {});
  if (line) st.line = line;
  if (url) st.url = url;
  if (finished != null) {
    st.result = finished;
    loadConnections();
  } else {
    renderConnections();
  }
});

listen("usage-updated", () => renderConnections());
loadConnections();

// ---------- Autostart ----------
const autostart = document.getElementById("autostart");
invoke("get_autostart").then((on) => (autostart.checked = on));
autostart.addEventListener("change", async () => {
  try {
    await invoke("set_autostart", { enabled: autostart.checked });
  } catch (err) {
    autostart.checked = !autostart.checked;
    alert(`Não foi possível alterar: ${err}`);
  }
});

// ---------- Widget background colour ----------
const widgetColor = document.getElementById("widget-color");
const widgetColorReset = document.getElementById("widget-color-reset");
function showWidgetColor(color) {
  // No custom colour: show the theme's default, dimmed, as the picker value.
  widgetColor.value = color ?? "#121218";
  widgetColor.classList.toggle("unset", !color);
  widgetColorReset.disabled = !color;
}
// "input" fires while dragging in the picker: live preview on the widget.
widgetColor.addEventListener("input", () => {
  invoke("set_widget_color", { color: widgetColor.value });
  showWidgetColor(widgetColor.value);
});
widgetColorReset.addEventListener("click", () => {
  invoke("set_widget_color", { color: null });
  showWidgetColor(null);
});
invoke("get_settings").then((s) => showWidgetColor(s.widget.color));

const widgetSessions = document.getElementById("widget-sessions");
widgetSessions.addEventListener("change", () => invoke("set_widget_sessions", { mode: widgetSessions.value }));
invoke("get_settings").then((s) => (widgetSessions.value = s.widget.sessions));
