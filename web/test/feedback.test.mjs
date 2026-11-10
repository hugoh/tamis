import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const SCRIPT = readFileSync(
  new URL("../static/feedback.js", import.meta.url),
  "utf8",
);

function page() {
  const handlers = {};
  const toasts = [];
  const timers = [];
  const triggered = [];

  const sandbox = {
    document: {
      getElementById: (id) =>
        id === "toasts" ? { append: (alert) => toasts.push(alert) } : null,
      createElement: () => ({
        removed: false,
        remove() {
          this.removed = true;
        },
      }),
      addEventListener: (name, handler) => {
        handlers[name] = handler;
      },
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
    fire: (name, detail) => handlers[name]({ detail }),
  };
}

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

test("a failed request raises an error toast that lingers for ten seconds", () => {
  const { fire, toasts, timers } = page();

  fire("htmx:afterRequest", {
    elt: button("Spam"),
    successful: false,
    xhr: { status: 500 },
  });

  assert.equal(toasts.length, 1);
  assert.equal(toasts[0].className, "alert alert-error");
  assert.equal(toasts[0].textContent, "The request failed (HTTP 500).");
  assert.equal(timers[0].delay, 10000);
});

for (const status of [0, 401, 403]) {
  test(`HTTP ${status} tells the user to reload and sign in again`, () => {
    const { fire, toasts } = page();

    fire("htmx:afterRequest", {
      elt: button("Spam"),
      successful: false,
      xhr: { status },
    });

    assert.match(toasts[0].textContent, /reload the page/);
  });
}

test("an action note becomes a success toast that lingers for five seconds and refreshes the list", () => {
  const { fire, toasts, timers, triggered } = page();

  fire(
    "htmx:afterRequest",
    respond("<tr><td><div data-note>Moved to Junk</div></td></tr>"),
  );

  assert.equal(toasts[0].className, "alert alert-success");
  assert.equal(toasts[0].textContent, "Moved to Junk");
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
  assert.equal(toasts[0].textContent, "The mailbox moved on");
  assert.deepEqual(triggered, []);
});

test("a response without a note raises no toast", () => {
  const { fire, toasts, triggered } = page();

  fire("htmx:afterRequest", respond("<tr><td>plain</td></tr>"));

  assert.deepEqual(toasts, []);
  assert.deepEqual(triggered, []);
});

test("a toast removes itself when its timer fires", () => {
  const { fire, toasts, timers } = page();
  fire("htmx:afterRequest", respond("<div data-note>Done</div>"));

  timers[0].callback();

  assert.equal(toasts[0].removed, true);
});
