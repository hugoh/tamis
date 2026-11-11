import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const SCRIPT = readFileSync(
  new URL("../static/feedback.js", import.meta.url),
  "utf8",
);

function element() {
  return {
    removed: false,
    className: "",
    textContent: "",
    children: [],
    listeners: {},
    append(...kids) {
      this.children.push(...kids);
    },
    addEventListener(name, handler) {
      this.listeners[name] = handler;
    },
    contains: () => false,
    remove() {
      this.removed = true;
    },
  };
}

function page({ ariaNotify = true } = {}) {
  const handlers = {};
  const toasts = [];
  const timers = [];
  const triggered = [];
  const announced = [];
  const status = { textContent: "" };
  const container = {
    popoverOpen: false,
    children: toasts,
    append: (alert) => toasts.push(alert),
    matches: () => container.popoverOpen,
    showPopover: () => {
      container.popoverOpen = true;
    },
    hidePopover: () => {
      container.popoverOpen = false;
    },
  };

  const sandbox = {
    document: {
      getElementById: (id) =>
        id === "toasts" ? container : id === "toast-status" ? status : null,
      createElement: element,
      querySelector: () => null,
      addEventListener: (name, handler) => {
        handlers[name] = handler;
      },
      ...(ariaNotify && { ariaNotify: (m) => announced.push(m) }),
    },
    DOMParser: class {
      parseFromString(source) {
        const note = /<div data-note( data-failed)?>([^<]*)<\/div>/.exec(
          source,
        );
        return {
          querySelector: () =>
            note && {
              textContent: note[2],
              hasAttribute: (name) => name === "data-failed" && !!note[1],
            },
        };
      }
    },
    setTimeout: (callback, delay) => timers.push({ callback, delay }),
    htmx: { trigger: (...args) => triggered.push(args) },
  };
  vm.runInNewContext(SCRIPT, sandbox);

  return {
    toasts,
    timers,
    triggered,
    announced,
    status,
    container,
    fire: (name, detail) => handlers[name]({ detail }),
  };
}

const message = (alert) => alert.children[0].textContent;

const button = (text) => ({
  tagName: "BUTTON",
  textContent: text,
  dataset: {},
});
const respond = (responseText, extra = {}) => ({
  elt: button("Spam"),
  successful: true,
  xhr: { status: 200, responseText },
  ...extra,
});

test("a button reads Working… while its request runs and gets its label back", () => {
  const { fire } = page();
  const elt = button("Spam");

  fire("htmx:beforeRequest", { elt });
  assert.equal(elt.textContent, "Working…");
  fire("htmx:afterRequest", {
    elt,
    successful: true,
    xhr: { responseText: "" },
  });

  assert.equal(elt.textContent, "Spam");
});

test("elements that are not buttons keep their text while a request runs", () => {
  const { fire } = page();
  const elt = { tagName: "FORM", textContent: "form", dataset: {} };

  fire("htmx:beforeRequest", { elt });

  assert.equal(elt.textContent, "form");
  assert.equal(elt.dataset.label, undefined);
});

test("a failed request raises an error toast that stays until closed", () => {
  const { fire, toasts, timers } = page();

  fire("htmx:afterRequest", {
    elt: button("Spam"),
    successful: false,
    xhr: { status: 500 },
  });

  assert.equal(toasts.length, 1);
  assert.equal(toasts[0].className, "alert alert-error");
  assert.equal(message(toasts[0]), "The request failed (HTTP 500).");
  assert.deepEqual(timers, []);
});

for (const status of [0, 401, 403]) {
  test(`HTTP ${status} tells the user to reload and sign in again`, () => {
    const { fire, toasts } = page();

    fire("htmx:afterRequest", {
      elt: button("Spam"),
      successful: false,
      xhr: { status },
    });

    assert.match(message(toasts[0]), /reload the page/);
  });
}

test("an action note becomes a success toast that lingers for five seconds and refreshes the list", () => {
  const { fire, toasts, timers, triggered } = page();

  fire(
    "htmx:afterRequest",
    respond("<tr><td><div data-note>Moved to Junk</div></td></tr>"),
  );

  assert.equal(toasts[0].className, "alert alert-success");
  assert.equal(message(toasts[0]), "Moved to Junk");
  assert.equal(timers[0].delay, 5000);
  assert.deepEqual(triggered, [["#toolbar", "submit"]]);
});

test("a failed action note becomes an error toast and leaves the list alone", () => {
  const { fire, toasts, triggered } = page();

  fire(
    "htmx:afterRequest",
    respond(
      "<tr><td><div data-note data-failed>The mailbox moved on</div></td></tr>",
    ),
  );

  assert.equal(toasts[0].className, "alert alert-error");
  assert.equal(message(toasts[0]), "The mailbox moved on");
  assert.deepEqual(triggered, []);
});

test("a response without a note raises no toast", () => {
  const { fire, toasts, triggered } = page();

  fire("htmx:afterRequest", respond("<tr><td>plain</td></tr>"));

  assert.deepEqual(toasts, []);
  assert.deepEqual(triggered, []);
});

test("a success toast removes itself when its timer fires and the region closes", () => {
  const { fire, toasts, timers, container } = page();
  fire("htmx:afterRequest", respond("<div data-note>Done</div>"));
  assert.equal(container.popoverOpen, true);

  timers[0].callback();

  assert.equal(toasts[0].removed, true);
});

test("a toast has a Close button that removes it", () => {
  const { fire, toasts } = page();
  fire("htmx:afterRequest", respond("<div data-note data-failed>Nope</div>"));
  const close = toasts[0].children[1];

  assert.equal(close.textContent, "Close");
  assert.equal(close.type, "button");
  close.listeners.click();

  assert.equal(toasts[0].removed, true);
});

test("toasts are announced with ariaNotify, errors prefixed", () => {
  const { fire, announced } = page();

  fire("htmx:afterRequest", respond("<div data-note>Done</div>"));
  fire("htmx:afterRequest", respond("<div data-note data-failed>Nope</div>"));

  assert.deepEqual(announced, ["Done", "Error: Nope"]);
});

test("without ariaNotify the message goes to the live region", () => {
  const { fire, status } = page({ ariaNotify: false });

  fire("htmx:afterRequest", respond("<div data-note>Done</div>"));

  assert.equal(status.textContent, "Done");
});
