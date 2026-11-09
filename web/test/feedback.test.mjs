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
  const assigned = [];

  const sandbox = {
    URL,
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
    location: { assign: (url) => assigned.push(url) },
  };
  vm.runInNewContext(SCRIPT, sandbox);

  return {
    toasts,
    timers,
    triggered,
    assigned,
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

test("navigating with a boosted link never raises a note toast", () => {
  const { fire, toasts, triggered } = page();

  fire(
    "htmx:afterRequest",
    respond("<main><div data-note>Old note</div></main>", { boosted: true }),
  );

  assert.deepEqual(toasts, []);
  assert.deepEqual(triggered, []);
});

test("a boosted navigation that fails still raises an error toast", () => {
  const { fire, toasts } = page();

  fire("htmx:afterRequest", {
    elt: { tagName: "A", dataset: {} },
    boosted: true,
    successful: false,
    xhr: { status: 500 },
  });

  assert.equal(toasts[0].className, "alert alert-error");
});

test("a boosted navigation redirected to the login page reloads instead of nesting it", () => {
  const { fire, assigned } = page();
  const detail = {
    boosted: true,
    shouldSwap: true,
    xhr: { responseURL: "https://tamis.test/login" },
  };

  fire("htmx:beforeSwap", detail);

  assert.equal(detail.shouldSwap, false);
  assert.deepEqual(assigned, ["https://tamis.test/login"]);
});

test("a boosted navigation to an ordinary page swaps normally", () => {
  const { fire, assigned } = page();
  const detail = {
    boosted: true,
    shouldSwap: true,
    xhr: { responseURL: "https://tamis.test/health" },
  };

  fire("htmx:beforeSwap", detail);

  assert.equal(detail.shouldSwap, true);
  assert.deepEqual(assigned, []);
});

test("a non-boosted request redirected to login is left to htmx", () => {
  const { fire } = page();
  const detail = {
    shouldSwap: true,
    xhr: { responseURL: "https://tamis.test/login" },
  };

  fire("htmx:beforeSwap", detail);

  assert.equal(detail.shouldSwap, true);
});
