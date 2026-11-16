// Filter chips: removing one resets its control in the toolbar and fires the
// control's own change/input event, so htmx reloads the results as usual.
document.addEventListener("click", (event) => {
  const chip = event.target.closest("button[data-clear]");
  if (!chip) return;
  const toolbar = document.getElementById("toolbar");
  const field = chip.dataset.clear;
  const controls = toolbar?.querySelectorAll(`[name="${field}"]`) ?? [];
  for (const control of controls) {
    if (control.type === "checkbox") {
      control.checked = false;
    } else if (control.type === "radio") {
      control.checked = control.value === "";
    } else {
      control.value = "";
    }
  }
  const target =
    field === "verdict"
      ? toolbar.querySelector('[name="verdict"][value=""]')
      : controls[0];
  target?.dispatchEvent(
    new Event(target.type === "search" ? "input" : "change", { bubbles: true }),
  );
});
