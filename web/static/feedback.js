// Toasts for every request htmx sends: what a decision action did, or why it
// failed, including when the request never reached tamis (session expired).
//
// The #toasts region is a manual popover (top layer, no light dismiss). Each
// toast has a Close button; errors stay until closed, successes clear
// themselves. Text is announced with ariaNotify() where it exists, otherwise
// through the #toast-status live region that is already in the page.
const toasts = () => document.getElementById("toasts");

const announce = (message) => {
  if (document.ariaNotify) {
    document.ariaNotify(message);
  } else {
    const status = document.getElementById("toast-status");
    if (status) status.textContent = message;
  }
};

const dismiss = (alert, trigger) => {
  const container = toasts();
  if (alert.contains(document.activeElement)) {
    // Keep focus somewhere sensible: the next toast, else where the user was.
    const next = alert.nextElementSibling ?? alert.previousElementSibling;
    (
      next?.querySelector("button") ??
      (trigger?.isConnected ? trigger : document.querySelector("main"))
    )?.focus();
  }
  alert.remove();
  if (container && !container.children.length && container.hidePopover) {
    if (container.matches(":popover-open")) container.hidePopover();
  }
};

const toast = (message, failed, trigger) => {
  const container = toasts();
  const alert = document.createElement("div");
  alert.className = `alert ${failed ? "alert-error" : "alert-success"}`;
  const text = document.createElement("span");
  text.textContent = message;
  const close = document.createElement("button");
  close.type = "button";
  close.className = "btn btn-ghost btn-xs";
  close.textContent = "Close";
  close.addEventListener("click", () => dismiss(alert, trigger));
  alert.append(text, close);
  container.append(alert);
  if (container.showPopover && !container.matches(":popover-open")) {
    container.showPopover();
  }
  announce(failed ? `Error: ${message}` : message);
  if (!failed) setTimeout(() => dismiss(alert, trigger), 5000);
};

const failure = (status) =>
  status === 0 || status === 401 || status === 403
    ? "Not signed in or the connection dropped: reload the page and try again."
    : `The request failed (HTTP ${status}).`;

const busyButton = (elt) =>
  elt.tagName === "FORM"
    ? elt.querySelector("button[type=submit]")
    : elt.tagName === "BUTTON"
      ? elt
      : null;

document.addEventListener("htmx:beforeRequest", (event) => {
  const button = busyButton(event.detail.elt);
  if (button) {
    button.dataset.label = button.textContent;
    button.style.width = `${button.getBoundingClientRect().width}px`;
    button.setAttribute("aria-busy", "true");
    const spinner = document.createElement("span");
    spinner.className = "loading loading-spinner loading-xs";
    spinner.setAttribute("aria-hidden", "true");
    const hidden = document.createElement("span");
    hidden.className = "sr-only";
    hidden.textContent = "Working…";
    button.replaceChildren(spinner, hidden);
  }
});

document.addEventListener("htmx:afterRequest", (event) => {
  const { elt, xhr, successful } = event.detail;
  const button = busyButton(elt);
  if (button?.dataset.label) {
    button.textContent = button.dataset.label;
    button.style.width = "";
    button.removeAttribute("aria-busy");
  }
  if (!successful) {
    toast(failure(xhr.status), true, elt);
    return;
  }
  const html = new DOMParser().parseFromString(
    `<table>${xhr.responseText}</table>`,
    "text/html",
  );
  const note = html.querySelector("[data-note]");
  if (!note) return;
  const failed = note.hasAttribute("data-failed");
  toast(note.textContent, failed, elt);
  if (!failed) htmx.trigger("#toolbar", "submit");
});
