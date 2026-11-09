// Toasts for every request htmx sends: what a decision action did, or why it
// failed, including when the request never reached tamis (session expired).
const toasts = () => document.getElementById("toasts");

const toast = (message, failed) => {
  const alert = document.createElement("div");
  alert.className = `alert ${failed ? "alert-error" : "alert-success"}`;
  alert.textContent = message;
  toasts().append(alert);
  setTimeout(() => alert.remove(), failed ? 10000 : 5000);
};

const failure = (status) =>
  status === 0 || status === 401 || status === 403
    ? "Not signed in or the connection dropped: reload the page and try again."
    : `The request failed (HTTP ${status}).`;

document.addEventListener("htmx:beforeRequest", (event) => {
  const button = event.detail.elt;
  if (button.tagName === "BUTTON") {
    button.dataset.label = button.textContent;
    button.textContent = "Working…";
  }
});

document.addEventListener("htmx:afterRequest", (event) => {
  const { elt, xhr, successful, boosted } = event.detail;
  if (elt.dataset.label) elt.textContent = elt.dataset.label;
  if (!successful) {
    toast(failure(xhr.status), true);
    return;
  }
  if (boosted) return;
  const html = new DOMParser().parseFromString(
    `<table>${xhr.responseText}</table>`,
    "text/html",
  );
  const note = html.querySelector("[data-note]");
  if (!note) return;
  const failed = note.hasAttribute("data-failed");
  toast(note.textContent, failed);
  if (!failed) htmx.trigger("#toolbar", "submit");
});

document.addEventListener("htmx:beforeSwap", (event) => {
  const { boosted, xhr } = event.detail;
  if (boosted && new URL(xhr.responseURL).pathname === "/login") {
    event.detail.shouldSwap = false;
    location.assign(xhr.responseURL);
  }
});
