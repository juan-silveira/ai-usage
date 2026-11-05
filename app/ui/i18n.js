// Translations from the app (crates/core/locales). Loaded first: the page's
// own script (data-main) starts only once the strings are here, so every
// render can call t() synchronously.
let I18N = { locale: "en", setting: "auto", locales: [], strings: {} };

function t(key, params = {}) {
  const s = I18N.strings[key] ?? key;
  return s.replace(/\{(\w+)\}/g, (m, name) => (name in params ? String(params[name]) : m));
}

/** Short weekday name for a Date, in the same words the tray uses. */
function weekdayShort(date) {
  return (t("weekdays_short").split(",")[(date.getDay() + 6) % 7] ?? "").trim();
}

/** Fills elements marked data-i18n (text), data-i18n-title, -placeholder, -aria-label. */
function applyI18n(root = document) {
  root.querySelectorAll("[data-i18n]").forEach((el) => (el.textContent = t(el.dataset.i18n)));
  for (const attr of ["title", "placeholder", "aria-label"]) {
    root.querySelectorAll(`[data-i18n-${attr}]`).forEach((el) =>
      el.setAttribute(attr, t(el.dataset[`i18n${attr === "aria-label" ? "AriaLabel" : attr[0].toUpperCase() + attr.slice(1)}`]))
    );
  }
}

(async () => {
  const main = document.currentScript.dataset.main;
  try {
    I18N = await window.__TAURI__.core.invoke("get_translations");
  } catch (err) {
    console.error("translations unavailable", err);
  }
  document.documentElement.lang = I18N.locale;
  applyI18n();
  // A language change re-runs everything with the new strings.
  window.__TAURI__.event.listen("language-changed", () => location.reload());
  const script = document.createElement("script");
  script.src = main;
  document.body.append(script);
})();
