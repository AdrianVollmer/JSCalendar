// Progressive enhancement for the settings form: mirrors saved prefs into
// localStorage so the form can restore them if the jscal_prefs cookie is
// ever cleared independently (see prefs.rs). The cookie remains the
// source of truth for what's actually rendered server-side.
(function () {
  var form = document.getElementById("settings-form");
  if (!form) return;
  var hasServerValue = form.dataset.hasOverride === "true";
  try {
    var saved = localStorage.getItem("jscal_prefs");
    if (saved && !hasServerValue) {
      var prefs = JSON.parse(saved);
      if (prefs.tz) form.timezone.value = prefs.tz;
      if (prefs.tf) form.time_format.value = prefs.tf;
      if (prefs.hr) form.holidays_region.value = prefs.hr;
    }
  } catch (e) {}
  form.addEventListener("submit", function () {
    try {
      localStorage.setItem("jscal_prefs", JSON.stringify({
        tz: form.timezone.value,
        tf: form.time_format.value,
        hr: form.holidays_region.value,
      }));
    } catch (e) {}
  });
})();
