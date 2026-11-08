import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const SCRIPT = readFileSync(
  new URL("../static/webauthn.js", import.meta.url),
  "utf8",
);

function page({ hash = "", search = "", buttons = [], respond } = {}) {
  const requests = [];
  const handlers = {};
  const fields = { name: { value: "passkey" } };
  const status = { textContent: "" };
  const location = { hash, search, href: "" };
  const credential = { toJSON: () => ({ id: "cred" }) };
  const created = [];

  const sandbox = {
    URLSearchParams,
    JSON,
    document: {
      getElementById(id) {
        if (id === "status") return status;
        if (fields[id]) return fields[id];
        if (!buttons.includes(id)) return null;
        return {
          addEventListener: (_event, handler) => {
            handlers[id] = handler;
          },
        };
      },
    },
    location,
    fetch: async (url, init) => {
      requests.push({ url, body: init.body, method: init.method });
      return (
        respond?.(url) ?? {
          ok: true,
          json: async () => ({ publicKey: { challenge: "c" } }),
        }
      );
    },
    PublicKeyCredential: {
      parseRequestOptionsFromJSON: (options) => ({ request: options }),
      parseCreationOptionsFromJSON: (options) => ({ creation: options }),
    },
    navigator: {
      credentials: {
        get: async () => credential,
        create: async (options) => {
          created.push(options);
          return credential;
        },
      },
    },
  };
  vm.runInNewContext(SCRIPT, sandbox);

  return {
    requests,
    created,
    status,
    location,
    fields,
    click: (id) => handlers[id](),
  };
}

const failure = (text, statusText = "Bad Request") => ({
  ok: false,
  statusText,
  text: async () => text,
});

test("enrolling sends the token from the URL fragment in the POST body", async () => {
  const site = page({ hash: "#token=a-b_c", buttons: ["enroll"] });

  await site.click("enroll");

  const [begin] = site.requests;
  assert.equal(begin.url, "/auth/register/begin");
  assert.equal(begin.method, "POST");
  assert.deepEqual(JSON.parse(begin.body), { token: "a-b_c" });
});

test("a token in the query string is not used", async () => {
  const site = page({ search: "?token=leaked", buttons: ["enroll"] });

  await site.click("enroll");

  assert.deepEqual(JSON.parse(site.requests[0].body), { token: "" });
});

test("enrolling finishes with the chosen name and goes to the home page", async () => {
  const site = page({ hash: "#token=t", buttons: ["enroll"] });
  site.fields.name.value = "my key&more";

  await site.click("enroll");

  const finish = site.requests[1];
  assert.equal(finish.url, "/auth/register/finish?name=my%20key%26more");
  assert.deepEqual(JSON.parse(finish.body), { id: "cred" });
  assert.deepEqual(JSON.parse(JSON.stringify(site.created)), [
    { publicKey: { creation: { challenge: "c" } } },
  ]);
  assert.equal(site.location.href, "/");
});

test("a refused enrolment shows the server's reason and stays put", async () => {
  const site = page({
    hash: "#token=old",
    buttons: ["enroll"],
    respond: () => failure("this link has expired\n"),
  });

  await site.click("enroll");

  assert.equal(site.status.textContent, "this link has expired");
  assert.equal(site.created.length, 0);
  assert.equal(site.location.href, "");
});

test("an error with no body falls back to the status text", async () => {
  const site = page({
    buttons: ["enroll"],
    respond: () => failure("", "Unauthorized"),
  });

  await site.click("enroll");

  assert.equal(site.status.textContent, "Unauthorized");
});

test("signing in begins, asks for a passkey, finishes and goes home", async () => {
  const site = page({ buttons: ["login"] });

  await site.click("login");

  assert.deepEqual(
    site.requests.map((r) => r.url),
    ["/auth/login/begin", "/auth/login/finish"],
  );
  assert.deepEqual(JSON.parse(site.requests[1].body), { id: "cred" });
  assert.equal(site.location.href, "/");
});

test("the login page has no enroll button and still loads", () => {
  assert.doesNotThrow(() => page({ buttons: ["login"] }));
});
