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

const STATUS_TEXT = ["no_credentials", "token_expired", "rate_limited"];
const LOCALE = I18N.locale;
const hhmm = (d) => d.toLocaleTimeString(LOCALE, { hour: "2-digit", minute: "2-digit" });

function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
}

function resetText(iso) {
  if (!iso) return "";
  const at = new Date(iso);
  const mins = Math.max(0, Math.round((at - Date.now()) / 60000));
  const d = Math.floor(mins / 1440), h = Math.floor((mins % 1440) / 60), m = mins % 60;
  const left = d > 0 ? `${d}d ${h}h` : `${h}h ${String(m).padStart(2, "0")}m`;
  return t("limits.resets", { left, when: `${weekdayShort(at)} ${hhmm(at)}` });
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
  const at = hhmm(new Date(p.fetched_at));
  const retry = p.stale.retry_at ? t("stale.retry_at", { at: hhmm(new Date(p.stale.retry_at)) }) : "";
  return `<p class="stale">${escapeHtml(t("stale.data_from", { at, reason: p.stale.reason }) + retry)}</p>`;
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
    const text = STATUS_TEXT.includes(p.status) ? t(`status.${p.status}`) : t("status.error", { message: p.message });
    body = `<p class="status">${escapeHtml(text)}</p>`;
  }
  return `<section class="card"><h2>${escapeHtml(p.display_name)} ${plan}</h2>${body}${staleNotice(p)}</section>`;
}

function renderLimits() {
  const root = document.getElementById("providers");
  if (latest.length === 0) return;
  const shown = latest.filter(
    (p) => p.status !== "no_credentials" && (activeTab === "all" || p.provider === activeTab)
  );
  root.innerHTML = shown.length
    ? shown.map(renderProvider).join("")
    : `<p class="muted">${escapeHtml(t("limits.none_for_provider"))}</p>`;
  const fresh = latest.filter((p) => !p.stale).map((p) => new Date(p.fetched_at));
  const at = fresh.length ? new Date(Math.max(...fresh)) : new Date(latest[0].fetched_at);
  document.getElementById("updated").textContent =
    t("limits.updated", { at: at.toLocaleTimeString(LOCALE), n: pollEvery });
}

function peakPercent(p) {
  return p && p.status === "ok" && p.windows.length
    ? Math.max(...p.windows.map((w) => w.used_percent))
    : null;
}

/** Providers with a sign-in (the rest only appear under Connections). */
function connectedIds() {
  return latest.filter((p) => p.status !== "no_credentials").map((p) => p.provider);
}

/** Providers known from either the usage poll or live sessions, in poll order. */
function providerIds() {
  const ids = connectedIds();
  for (const s of sessions) if (!ids.includes(s.provider)) ids.push(s.provider);
  return ids;
}

function renderTabs() {
  const ids = providerIds();
  if (activeTab !== "all" && activeTab !== "settings" && !ids.includes(activeTab) && latest.length) activeTab = "all";
  // Logo + name; with many providers in a narrow window the CSS hides the
  // names (the tooltip and accessible label keep them).
  document.getElementById("tabs").classList.toggle("many", ids.length > 2);
  const tab = (id, label, extra, icon) => `
    <button role="tab" class="tab" data-tab="${escapeHtml(id)}" title="${escapeHtml(label)}" aria-label="${escapeHtml(label)}"
      aria-selected="${id === activeTab}" tabindex="${id === activeTab ? 0 : -1}">
      ${icon ? `<img src="${icon}" alt="" />` : ""}<span class="name">${escapeHtml(label)}</span>${extra}
    </button>`;
  const waiting = (id) =>
    sessions.filter((s) => s.state === "needs_input" && (id === "all" || s.provider === id)).length;
  const alertBadge = (n) => (n ? `<span class="count" title="${escapeHtml(t("tabs.waiting", { n }))}">${n}</span>` : "");

  document.getElementById("tabs").innerHTML =
    tab("all", t("tabs.all"), alertBadge(waiting("all"))) +
    ids
      .map((id) => {
        const usage = latest.find((p) => p.provider === id);
        const pct = peakPercent(usage);
        const pctBadge = pct == null ? "" : `<span class="pct ${worstSeverity(usage)}">${pct.toFixed(0)}%</span>`;
        return tab(id, usage?.display_name ?? PROVIDER_NAME[id] ?? id, pctBadge + alertBadge(waiting(id)), PROVIDER_ICON[id]);
      })
      .join("") +
    `<button role="tab" class="tab gear" data-tab="settings" title="${escapeHtml(t("tabs.settings"))}" aria-label="${escapeHtml(t("tabs.settings"))}"
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

const PROVIDER_NAME = { claude: "Claude", codex: "Codex", copilot: "Copilot", cursor: "Cursor", openrouter: "OpenRouter" };

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
    ? t("session.turn", { n: Math.max(1, Math.round(s.last_turn_secs / 60)) })
    : "");
  return `
    <div class="session ${s.state}${acknowledged.get(key) === s.updated_at ? " acknowledged" : ""}" data-key="${escapeHtml(key)}" title="${escapeHtml(s.cwd)}">
      <div class="main">
        <div class="top">
          <span class="project">${escapeHtml(s.project)}</span>
          <span class="badge"><span class="dot">●</span> ${escapeHtml(t(`session.state.${s.state}`))} · ${ago(since)}</span>
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
    : `<p class="muted">${escapeHtml(t("sessions.none"))}</p>`;
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
  renderWidgetProviders();
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

const PROVIDER_ICON = { claude: "icons/claude.png", codex: "icons/codex.png", copilot: "icons/copilot.png", cursor: "icons/cursor.png", openrouter: "icons/openrouter.png" };
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
        const hm = at.toLocaleTimeString(LOCALE, { hour: "2-digit", minute: "2-digit", hour12: false });
        when = secs < 86400 ? hm : `${weekdayShort(at)} ${hm}`;
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
  const ids = connectedIds();
  document.getElementById("provider-bar").innerHTML = Object.entries(barSettings.providers)
    .filter(([id]) => ids.includes(id))
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
          <label><input type="checkbox" data-field="show_icon" ${cfg.show_icon ? "checked" : ""} /> ${escapeHtml(t("bar.icon"))}</label>
          <label><input type="checkbox" data-field="show_name" ${cfg.show_name ? "checked" : ""} /> ${escapeHtml(t("bar.name"))}</label>
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
  if (u) document.getElementById("update-text").textContent = t("update.banner", { version: u.version });
  const { current, error } = status;
  updateStatus.textContent = error
    ? t("update.status_error", { current, error })
    : u
      ? t("update.status_available", { current, version: u.version })
      : t("update.status_none", { current });
}
// Installs in place (signed bundle, then restart); where that isn't possible
// (e.g. .deb/.rpm installs) the release page opens instead.
const updateOpen = document.getElementById("update-open");
updateOpen.addEventListener("click", async () => {
  updateOpen.disabled = true;
  try {
    await invoke("install_update");
  } catch (err) {
    console.warn("in-place update failed", err);
    document.getElementById("update-text").textContent = t("update.install_failed");
    invoke("open_update");
  }
  updateOpen.disabled = false;
});
listen("update-progress", (e) => {
  const [done, total] = e.payload;
  if (total) document.getElementById("update-text").textContent =
    t("update.downloading", { pct: Math.round((done / total) * 100) });
});
document.getElementById("check-now").addEventListener("click", async () => {
  updateStatus.textContent = t("update.checking");
  showUpdate(await invoke("check_updates_now"));
});
const pollMinutes = document.getElementById("poll-minutes");
for (const opt of pollMinutes.options)
  opt.textContent = t(opt.value === "5" ? "polling.recommended" : "polling.minutes", { n: opt.value });
let pollEvery = 5;
pollMinutes.addEventListener("change", () => {
  pollEvery = Number(pollMinutes.value);
  invoke("set_poll_minutes", { minutes: pollEvery });
  render();
});

const checkUpdates = document.getElementById("check-updates");
checkUpdates.addEventListener("change", () => invoke("set_check_updates", { enabled: checkUpdates.checked }));
// The periodic check only sends the update found; keep the version we know.
let currentVersion = "";
listen("update-status", (e) => showUpdate({ current: currentVersion, update: e.payload, error: null }));
invoke("get_update").then((status) => {
  currentVersion = status.current;
  showUpdate(status);
});

const widgetEnabled = document.getElementById("widget-enabled");
const widgetMove = document.getElementById("widget-move");
const widgetOpacity = document.getElementById("widget-opacity");
let widgetMoving = false;

function renderWidgetControls() {
  widgetMove.disabled = !widgetEnabled.checked;
  widgetMove.textContent = t(widgetMoving ? "widget.pin" : "widget.move");
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
  renderWidgetProviders();
  renderConnections();
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
  if (!p) return { text: t("conn.checking"), cls: "" };
  switch (p.status) {
    case "ok":
      return { text: p.plan ? t("conn.connected_plan", { plan: p.plan }) : t("conn.connected"), cls: "ok" };
    case "token_expired":
      return { text: t("conn.expired"), cls: "bad" };
    case "no_credentials":
      return { text: t("conn.not_connected"), cls: "bad" };
    case "rate_limited":
      return { text: t("conn.rate_limited"), cls: "ok" };
    default:
      return { text: t("conn.error"), cls: "bad" };
  }
}

function renderConnections() {
  const root = document.getElementById("connections");
  if (!connections.length) return;
  if (root.contains(document.activeElement) && document.activeElement.tagName === "INPUT"
      && ["text", "password"].includes(document.activeElement.type)) return; // don't wipe a code or key being typed
  root.innerHTML = connections
    .map((c) => {
      if (c.kind !== "cli") return renderOtherConnection(c);
      const st = connStatus(c.provider);
      const icon = PROVIDER_ICON[c.provider];
      const login = loginState[c.provider] ?? {};
      const connected = st.cls === "ok";
      let action;
      if (!c.cli_path) {
        action = `<span class="state bad">${escapeHtml(t("conn.cli_missing"))}</span>`;
      } else if (c.login_running) {
        action = `<button class="text" data-act="cancel">${escapeHtml(t("conn.cancel"))}</button>`;
      } else {
        const expired = latest.find((x) => x.provider === c.provider)?.status === "token_expired";
        action = `<button class="text" data-act="login">${escapeHtml(t(connected || expired ? "conn.reconnect" : "conn.connect"))}</button>`;
        if (connected || expired)
          action = `<div class="actions">${action}<button class="text" data-act="logout">${escapeHtml(t("conn.disconnect"))}</button></div>`;
      }
      const loginBox = c.login_running
        ? `<div class="login">
            <span>${escapeHtml(t("conn.finish_in_browser"))}</span>
            ${login.url ? `<span>${escapeHtml(t("conn.no_browser"))} <a href="#" data-act="open-url">${escapeHtml(t("conn.open_link"))}</a></span>` : ""}
            <div class="row">
              <input type="text" data-field="code" placeholder="${escapeHtml(t("conn.code_placeholder"))}" />
              <button class="text" data-act="send-code">${escapeHtml(t("conn.send"))}</button>
            </div>
            ${login.line ? `<span class="line">${escapeHtml(login.line)}</span>` : ""}
          </div>`
        : login.result === false
          ? `<div class="login"><span>${escapeHtml(t("conn.login_failed"))}</span>${login.line ? `<span class="line">${escapeHtml(login.line)}</span>` : ""}</div>`
          : "";
      const logoutBox = login.confirmLogout
        ? `<div class="login">
            <span>${escapeHtml(t("conn.logout_confirm", { name: c.name })).replace(
              "{command}",
              `<code>${escapeHtml(c.provider === "claude" ? "claude auth logout" : "codex logout")}</code>`
            )}</span>
            <div class="row">
              <button class="text" data-act="logout-confirm">${escapeHtml(t("conn.disconnect"))}</button>
              <button class="text" data-act="logout-cancel">${escapeHtml(t("conn.cancel"))}</button>
            </div>
          </div>`
        : login.logoutError
          ? `<div class="login"><span>${escapeHtml(t("conn.logout_failed"))}</span><span class="line">${escapeHtml(login.logoutError)}</span></div>`
          : "";
      // Install steps when the CLI is missing; npm-based ones first need
      // Node.js, which a fresh machine often lacks.
      const node = c.node_install == null
        ? ""
        : c.node_install
          ? `<span class="muted">${escapeHtml(t("conn.needs_node"))}</span>
             <div class="row"><code>${escapeHtml(c.node_install)}</code>
               <button class="text" data-act="copy-node">${escapeHtml(t("conn.copy"))}</button></div>
             <span class="muted">${escapeHtml(t("conn.then"))}</span>`
          : `<span class="muted">${escapeHtml(t("conn.needs_node_link"))}</span>`;
      const install = c.cli_path
        ? ""
        : `${node}<div class="row"><code>${escapeHtml(c.install_command)}</code>
             <button class="text" data-act="copy">${escapeHtml(t("conn.copy"))}</button></div>`;
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
          ${escapeHtml(t("conn.hooks"))}</span></label>
      </div>`;
    })
    .join("");
}

/** Providers signed in elsewhere (Copilot, Cursor) or with an API key (OpenRouter). */
function renderOtherConnection(c) {
  const st = c.disabled ? { text: t("conn.disabled"), cls: "bad" } : connStatus(c.provider);
  const icon = PROVIDER_ICON[c.provider];
  const login = loginState[c.provider] ?? {};
  // Copilot/Cursor: "disconnect" stops tracking here, their sign-in stays.
  let action = "";
  if (c.kind === "app") {
    if (c.disabled) action = `<button class="text" data-act="enable">${escapeHtml(t("conn.connect"))}</button>`;
    else if (st.cls === "ok") action = `<button class="text" data-act="disable">${escapeHtml(t("conn.disconnect"))}</button>`;
  }
  const confirm = login.confirmDisable
    ? `<div class="login">
        <span>${escapeHtml(t("conn.app_disconnect_confirm", { name: c.name }))}</span>
        <div class="row">
          <button class="text" data-act="disable-confirm">${escapeHtml(t("conn.disconnect"))}</button>
          <button class="text" data-act="logout-cancel">${escapeHtml(t("conn.cancel"))}</button>
        </div>
      </div>`
    : "";
  let extra;
  if (c.kind === "key") {
    extra = c.key_source === "env"
      ? `<span class="muted">${escapeHtml(t("conn.key_from_env"))}</span>`
      : `<div class="row">
          <input type="password" data-field="key" autocomplete="off"
            placeholder="${escapeHtml(t(c.key_source ? "conn.key_saved" : "conn.key_placeholder"))}" />
          <button class="text" data-act="save-key">${escapeHtml(t("conn.save"))}</button>
          ${c.key_source ? `<button class="text" data-act="remove-key">${escapeHtml(t("conn.remove"))}</button>` : ""}
        </div>`;
  } else {
    extra = `<span class="muted">${escapeHtml(t(`conn.hint.${c.provider}`))}</span>`;
  }
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
      ${confirm}${c.disabled ? "" : extra}
    </div>`;
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
  } else if (act === "disable") {
    loginState[provider] = { confirmDisable: true };
    renderConnections();
  } else if (act === "disable-confirm" || act === "enable") {
    loginState[provider] = {};
    await invoke("set_provider_enabled", { provider, enabled: act === "enable" });
    await loadConnections();
  } else if (act === "save-key" || act === "remove-key") {
    const input = row.querySelector('[data-field="key"]');
    const key = act === "save-key" ? input.value.trim() : "";
    if (act === "save-key" && !key) return;
    await invoke("set_openrouter_key", { key });
    input.value = "";
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
  } else if (act === "copy" || act === "copy-node") {
    navigator.clipboard?.writeText(act === "copy" ? conn.install_command : conn.node_install);
    e.target.textContent = t("conn.copied");
  }
});

document.getElementById("connections").addEventListener("change", async (e) => {
  if (e.target.dataset.act !== "hooks") return;
  const provider = e.target.closest(".conn").dataset.provider;
  try {
    await invoke("set_hooks", { provider, enabled: e.target.checked });
  } catch (err) {
    alert(t("conn.hooks_error", { error: err }));
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

// A CLI installed while the app runs shows up without restarting it: check
// again when the window regains focus, and every few seconds while the
// preferences are open and a CLI is still missing.
window.addEventListener("focus", loadConnections);
setInterval(() => {
  if (activeTab === "settings" && connections.some((c) => c.kind === "cli" && !c.cli_path)) loadConnections();
}, 4000);

// ---------- Autostart ----------
const autostart = document.getElementById("autostart");
invoke("get_autostart").then((on) => (autostart.checked = on));
autostart.addEventListener("change", async () => {
  try {
    await invoke("set_autostart", { enabled: autostart.checked });
  } catch (err) {
    autostart.checked = !autostart.checked;
    alert(t("general.autostart_error", { error: err }));
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

// ---------- Language ----------
const language = document.getElementById("language");
language.innerHTML =
  `<option value="auto">${escapeHtml(t("language.auto"))}</option>` +
  I18N.locales.map(([code, name]) => `<option value="${escapeHtml(code)}">${escapeHtml(name)}</option>`).join("");
language.value = I18N.setting;
// The backend emits "language-changed", which reloads this page (i18n.js).
language.addEventListener("change", () => invoke("set_language", { language: language.value }));

// Providers shown in the widget.
const widgetProviders = document.getElementById("widget-providers");
let widgetShown = [];
function renderWidgetProviders() {
  widgetProviders.innerHTML = connectedIds()
    .map((id) => `<label><input type="checkbox" data-provider="${escapeHtml(id)}" ${widgetShown.includes(id) ? "checked" : ""} />
      ${PROVIDER_ICON[id] ? `<img src="${PROVIDER_ICON[id]}" alt="" />` : ""}${escapeHtml(PROVIDER_NAME[id] ?? id)}</label>`)
    .join("");
}
widgetProviders.addEventListener("change", (e) => {
  const id = e.target.dataset.provider;
  widgetShown = widgetShown.filter((p) => p !== id).concat(e.target.checked ? [id] : []);
  invoke("set_widget_provider", { provider: id, show: e.target.checked });
});
invoke("get_settings").then((s) => {
  widgetShown = s.widget.providers;
  renderWidgetProviders();
});

// ---------- Focus mode ----------
const muteSelect = document.getElementById("mute");
const muteStatus = document.getElementById("mute-status");
const muteBanner = document.getElementById("mute-banner");
for (const opt of muteSelect.options)
  if (/^\d+$/.test(opt.value))
    opt.textContent = Number(opt.value) < 60
      ? t("polling.minutes", { n: opt.value })
      : t("focus.hours", { n: opt.value / 60 });
let mute = { mode: "off" };

function muteActive() {
  return mute.mode === "always" || (mute.mode === "until" && new Date(mute.until) > Date.now());
}

function muteText() {
  if (mute.mode === "always") return t("focus.muted");
  const at = new Date(mute.until);
  const sameDay = at.toDateString() === new Date().toDateString();
  return t("focus.muted_until", { when: sameDay ? hhmm(at) : `${weekdayShort(at)} ${hhmm(at)}` });
}

function renderMute() {
  const on = muteActive();
  muteBanner.hidden = !on;
  muteStatus.hidden = !on;
  if (on) document.getElementById("mute-text").textContent = muteStatus.textContent = `🔕 ${muteText()}`;
  // While muted the select shows the state ("Muted until 15:30") through a
  // hidden option; picking any real option changes it.
  let current = muteSelect.querySelector('option[value="current"]');
  if (!current) {
    current = new Option("", "current");
    current.hidden = true;
    muteSelect.prepend(current);
  }
  current.textContent = on ? muteText() : "";
  muteSelect.value = on ? "current" : "off";
}

muteSelect.addEventListener("change", async () => {
  mute = await invoke("set_mute", { mode: muteSelect.value });
  renderMute();
});
document.getElementById("mute-off").addEventListener("click", async () => {
  mute = await invoke("set_mute", { mode: "off" });
  renderMute();
});
listen("mute-updated", (e) => {
  mute = e.payload;
  renderMute();
});
setInterval(renderMute, 30_000);

// ---------- Sounds: tone per alert and volume ----------
const TONES = ["bell", "chime", "soft", "ping", "pulse"];
const toneSelects = document.querySelectorAll("select.tone");
toneSelects.forEach((sel) => {
  sel.innerHTML = TONES.map((tone) => `<option value="${tone}">${escapeHtml(t(`tone.${tone}`))}</option>`).join("");
  sel.addEventListener("change", () => invoke("set_sound_tone", { sound: sel.dataset.sound, tone: sel.value }));
});
const soundVolume = document.getElementById("sound-volume");
soundVolume.addEventListener("change", () => {
  invoke("set_sound_volume", { volume: Number(soundVolume.value) });
  invoke("preview_sound", { sound: "done" });
});

invoke("get_settings").then((s) => {
  mute = s.mute;
  renderMute();
  toneSelects.forEach((sel) => (sel.value = s.sound_tones[sel.dataset.sound]));
  soundVolume.value = s.sound_volume;
});

// ---------- Local API ----------
const apiEnabled = document.getElementById("api-enabled");
const apiPort = document.getElementById("api-port");
const apiInfo = document.getElementById("api-info");
let apiError = "";
function renderApi() {
  apiPort.disabled = !apiEnabled.checked;
  const base = `http://127.0.0.1:${apiPort.value}`;
  apiInfo.innerHTML = apiError
    ? escapeHtml(t("api.error", { error: apiError }))
    : apiEnabled.checked
      ? escapeHtml(t("api.running")) + ` <code>${base}/v1/status</code> · <code>/v1/usage</code> · <code>/v1/sessions</code> · <code>/v1/events</code>`
      : escapeHtml(t("api.note"));
}
async function saveApi() {
  try {
    await invoke("set_api", { enabled: apiEnabled.checked, port: Number(apiPort.value) || 0 });
    apiError = "";
  } catch (err) {
    apiError = String(err);
    if (apiEnabled.checked) apiEnabled.checked = false;
  }
  renderApi();
}
apiEnabled.addEventListener("change", saveApi);
apiPort.addEventListener("change", saveApi);
invoke("get_settings").then((s) => {
  apiEnabled.checked = s.api_enabled;
  apiPort.value = s.api_port;
  renderApi();
});
