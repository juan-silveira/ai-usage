const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

let latest = [];

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

listen("usage-updated", (event) => {
  latest = event.payload;
  render();
});

document.getElementById("refresh").addEventListener("click", () => invoke("refresh_now"));

// Keep countdowns ticking between polls.
setInterval(render, 30_000);

invoke("get_usage").then((usage) => {
  latest = usage;
  render();
});
