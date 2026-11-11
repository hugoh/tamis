import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const SCRIPT = readFileSync(
  new URL("../static/whitelist.js", import.meta.url),
  "utf8",
);

function page() {
  const handlers = {};
  const sandbox = {
    document: {
      getElementById: () => null,
      querySelectorAll: () => [],
      addEventListener: (name, handler, capture) => {
        handlers[name] = { handler, capture };
      },
    },
    htmx: {
      onLoad: (handler) => {
        handlers.onLoad = handler;
      },
    },
  };
  vm.runInNewContext(SCRIPT, sandbox);
  return handlers;
}

// New content as htmx hands it over: a container that may hold dialogs.
const content = (...dialogs) => ({
  matches: () => false,
  querySelectorAll: () => dialogs,
});

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

  page().onLoad(content(confirm));

  assert.equal(confirm.shown, 1);
});

test("swapped-in content that is itself the dialog is opened", () => {
  const confirm = dialog();
  confirm.querySelectorAll = () => [];

  page().onLoad(confirm);

  assert.equal(confirm.shown, 1);
});

test("a dialog that is already open is not opened twice", () => {
  const confirm = dialog(true);

  page().onLoad(content(confirm));

  assert.equal(confirm.shown, 0);
});

test("new content without a dialog is left alone", () => {
  assert.doesNotThrow(() => page().onLoad(content()));
});

test("closing the dialog removes it, listening in the capture phase", () => {
  const confirm = dialog();
  const handlers = page();

  assert.equal(handlers.close.capture, true);
  handlers.close.handler({ target: confirm });

  assert.equal(confirm.removed, true);
});

test("close events from other elements are ignored", () => {
  const other = { ...dialog(), matches: () => false };

  page().close.handler({ target: other });

  assert.equal(other.removed, false);
});
