// Minimal progressive-enhancement JS: everything above this file (routing,
// forms, navigation) already works without it via plain links/forms + htmx.

(function serviceWorker() {
  if ("serviceWorker" in navigator) {
    window.addEventListener("load", () => {
      navigator.serviceWorker.register("/sw.js").catch(() => {});
    });
  }
})();

(function theme() {
  const root = document.documentElement;
  const stored = localStorage.getItem("jscal-theme");
  if (stored) root.setAttribute("data-theme", stored);

  document.addEventListener("click", (e) => {
    const toggle = e.target.closest("#theme-toggle");
    if (!toggle) return;
    const prefersDark = window.matchMedia("(prefers-color-scheme: dark)").matches;
    const current = root.getAttribute("data-theme") || (prefersDark ? "dark" : "light");
    const next = current === "dark" ? "light" : "dark";
    root.setAttribute("data-theme", next);
    localStorage.setItem("jscal-theme", next);
  });
})();

(function modalDismiss() {
  document.addEventListener("click", (e) => {
    if (e.target.id === "event-dialog-backdrop") {
      document.getElementById("modal-root").innerHTML = "";
    }
  });
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && document.querySelector(".event-dialog")) {
      document.getElementById("modal-root").innerHTML = "";
    }
  });
})();

(function confirmSubmit() {
  // Progressive-enhancement confirmation for forms marked data-confirm
  // (e.g. admin user deletion) — without JS the form just submits, same
  // as the hx-confirm-driven delete buttons elsewhere in the app already
  // behave with JS disabled.
  document.addEventListener("submit", (e) => {
    const msg = e.target.getAttribute && e.target.getAttribute("data-confirm");
    if (msg && !confirm(msg)) e.preventDefault();
  });
})();

(function allDayToggle() {
  document.addEventListener("change", (e) => {
    if (e.target.matches && e.target.matches('input[name="all_day"]')) {
      const form = e.target.closest("form");
      if (form) form.classList.toggle("all-day", e.target.checked);
    }
  });
})();

(function scrollTimeGrid() {
  function scrollToRelevantHour() {
    const scroller = document.querySelector(".time-grid-scroll");
    if (!scroller) return;
    const now = new Date();
    const isToday = document.querySelector(".time-day-header.today");
    const hour = isToday ? Math.max(now.getHours() - 1, 0) : 8;
    const target = document.getElementById("hour-" + hour);
    if (target) scroller.scrollTop = target.offsetTop;
  }
  document.addEventListener("DOMContentLoaded", scrollToRelevantHour);
  document.body.addEventListener("htmx:afterSwap", scrollToRelevantHour);
})();
