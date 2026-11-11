// The header checkbox ticks every address loaded so far, and rows that scroll in
// afterwards while it is ticked. Unticking a row unticks the header.
const rowBoxes = () =>
  document.querySelectorAll("input[name=address][form=bulk]");
const header = () => document.getElementById("select-all");

document.addEventListener("change", (event) => {
  if (event.target.id === "select-all") {
    for (const box of rowBoxes()) box.checked = event.target.checked;
  } else if (
    event.target.name === "address" &&
    !event.target.checked &&
    header()
  ) {
    header().checked = false;
  }
});

// The server renders the confirmation as a plain <dialog data-modal>; opening it
// as a modal gives the inert page, focus containment, Esc and focus return.
// htmx.onLoad runs for each piece of new content (the page, then every swap),
// so only what just arrived is looked at.
htmx.onLoad((content) => {
  const dialogs = content.matches?.("dialog[data-modal]")
    ? [content]
    : (content.querySelectorAll?.("dialog[data-modal]") ?? []);
  for (const dialog of dialogs) {
    if (!dialog.open) dialog.showModal();
  }
});

document.addEventListener("htmx:afterSettle", () => {
  if (header()?.checked) {
    for (const box of rowBoxes()) box.checked = true;
  }
});

// Cancel and Esc close the dialog natively; drop it so a stale copy never lingers.
// `close` does not bubble, hence the capture phase.
document.addEventListener(
  "close",
  (event) => {
    if (event.target.matches?.("dialog[data-modal]")) event.target.remove();
  },
  true,
);
