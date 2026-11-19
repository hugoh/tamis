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

// Places the Hide menu under its button, kept inside the viewport. The menu is
// in the top layer, so it is positioned from the button's rectangle instead of
// CSS anchor positioning, which Safari puts at the page's top left.
document.getElementById("hide-menu")?.addEventListener("toggle", (event) => {
  if (event.newState !== "open") return;
  const menu = event.target;
  const button = document.querySelector('[popovertarget="hide-menu"]');
  const box = button.getBoundingClientRect();
  const margin = 8;
  const farthest =
    document.documentElement.clientWidth - menu.offsetWidth - margin;
  Object.assign(menu.style, {
    position: "fixed",
    top: `${box.bottom + 4}px`,
    left: `${Math.max(margin, Math.min(box.left, farthest))}px`,
  });
});
