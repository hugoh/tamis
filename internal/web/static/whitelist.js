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

document.addEventListener("htmx:afterSettle", () => {
  if (header()?.checked) {
    for (const box of rowBoxes()) box.checked = true;
  }
});
