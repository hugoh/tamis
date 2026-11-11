import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const SCRIPT = readFileSync(
  new URL("../static/whitelist.js", import.meta.url),
  "utf8",
);

function page(dialogs) {
  const handlers = {};
  const sandbox = {
    document: {
      getElementById: () => null,
      querySelectorAll: (selector) =>
        selector.startsWith("dialog") ? dialogs : [],
      addEventListener: (name, handler, capture) => {
        handlers[name] = { handler, capture };
      },
    },
  };
  vm.runInNewContext(SCRIPT, sandbox);
  return handlers;
}

const dialog = (open = false) => ({
  open,
  shown: 0,
  removed: false,
  showModal() {
    this.shown += 1;
    this.open = true;
  },
  remove() {
    this.removed = true;
  },
  matches: (selector) => selector === "dialog[data-modal]",
});

test("a confirmation swapped in by htmx is opened as a modal", () => {
  const confirm = dialog();
  const handlers = page([confirm]);

  handlers["htmx:afterSettle"].handler();

  assert.equal(confirm.shown, 1);
});

test("a dialog that is already open is not opened twice", () => {
  const confirm = dialog(true);
  const handlers = page([confirm]);

  handlers["htmx:afterSettle"].handler();

  assert.equal(confirm.shown, 0);
});

test("closing the dialog removes it, listening in the capture phase", () => {
  const confirm = dialog();
  const handlers = page([confirm]);

  assert.equal(handlers.close.capture, true);
  handlers.close.handler({ target: confirm });

  assert.equal(confirm.removed, true);
});

test("close events from other elements are ignored", () => {
  const other = { ...dialog(), matches: () => false };
  const handlers = page([]);

  handlers.close.handler({ target: other });

  assert.equal(other.removed, false);
});
