import assert from "node:assert/strict";
import { after, before, test } from "node:test";
import { startMock } from "./harness.mjs";

let mock;
before(async () => {
  mock = await startMock();
});
after(() => mock?.stop());

const row = (page, subject) =>
  page.locator("tbody tr", { hasText: new RegExp(`${subject}\\b`) });

test("the list shows a row per decision with state, pills and a destination badge", async () => {
  const page = await mock.page();
  await page.goto(`${mock.origin}/`);
  await page.waitForSelector("tbody tr[data-state]");
  const states = (selector) =>
    page.locator(selector).evaluateAll((rows) => [...new Set(rows.map((r) => r.dataset.state))].sort());
  assert.deepEqual(await states("tbody tr[data-state]"), ["good", "spam", "uncertain"]);
  const moved = row(page, "Sample message 150");
  assert.match(await moved.locator("[data-dest]").getAttribute("class"), /\bbadge-neutral\b/);

  // The hand-written rows are older than the first page: reach them by search.
  await page.goto(`${mock.origin}/?reason=rule`);
  await page.waitForSelector("tbody tr[data-state=rule]");
  await page.goto(`${mock.origin}/?q=Lunch`);
  await page.waitForSelector("tbody tr[data-state=corrected]");
  assert.match(
    await page.locator("tr[data-state=corrected] [data-dest]").first().getAttribute("class"),
    /\bbadge-outline\b/,
  );
});

test("scrolling to the end of the list loads the next page", async () => {
  const page = await mock.page();
  await page.goto(`${mock.origin}/`);
  const rows = page.locator("tbody tr[data-state]");
  await rows.first().waitFor();
  const first = await rows.count();

  await page.locator("tbody tr").last().scrollIntoViewIfNeeded();

  await page.waitForFunction(
    (n) => document.querySelectorAll("tbody tr[data-state]").length > n,
    first,
  );
});

test("searching narrows the list and the URL keeps the search", async () => {
  const page = await mock.page();
  await page.goto(`${mock.origin}/`);

  await page.getByRole("searchbox", { name: "Search" }).fill("Sample message 150");

  await page.waitForURL(/q=Sample\+message\+150|q=Sample%20message%20150/);
  await page.locator("tbody tr[data-state]").first().waitFor();
  assert.equal(await page.locator("tbody tr[data-state]").count(), 1);
  await page.getByRole("searchbox", { name: "Search" }).fill("nothing matches this");
  await page.getByText(/No decisions match/).waitFor();
});

test("the verdict filter keeps only that verdict", async () => {
  const page = await mock.page();
  await page.goto(`${mock.origin}/`);

  await page.getByRole("radio", { name: "spam" }).check();

  await page.waitForURL(/verdict=spam/);
  await page.waitForFunction(
    () => !document.querySelector("tbody tr[data-state=good]"),
  );
  assert.ok((await page.locator("tbody tr[data-state=spam]").count()) > 0);
});

test("a column heading sorts the list and flips on a second click", async () => {
  const page = await mock.page();
  await page.goto(`${mock.origin}/`);

  await page.getByRole("link", { name: "Score" }).click();
  await page.waitForURL(/sort=score/);
  const header = page.locator("th", { has: page.getByRole("link", { name: /Score/ }) });
  const first = await header.getAttribute("aria-sort");
  assert.notEqual(first, "none");

  await page.getByRole("link", { name: /Score/ }).click();
  await page.waitForFunction(
    (before) =>
      [...document.querySelectorAll("th")].find((th) => /Score/.test(th.textContent))
        ?.getAttribute("aria-sort") !== before,
    first,
  );
});

test("Undo shows a toast, reloads the row as corrected and the toast can be closed", async () => {
  const page = await mock.page();
  await page.goto(`${mock.origin}/`);
  const target = row(page, "Sample message 150");

  await target.getByRole("button", { name: "Undo" }).click();

  const toast = page.locator("#toasts .alert-success");
  await toast.getByText("moved back to INBOX").waitFor();
  // The undo is journalled as a row of its own; the original dims.
  await page.waitForFunction(() =>
    [...document.querySelectorAll("tbody tr[data-state=corrected]")].some((r) =>
      /Sample message 150\b/.test(r.textContent),
    ),
  );
  await toast.getByRole("button", { name: "Close" }).click();
  await toast.waitFor({ state: "detached" });
});

test("a success toast clears itself after a few seconds", async () => {
  const page = await mock.page();
  await page.clock.install();
  await page.goto(`${mock.origin}/`);

  await row(page, "Sample message 138").getByRole("button", { name: "Undo" }).click();

  const toast = page.locator("#toasts .alert-success");
  await toast.waitFor();
  await page.clock.fastForward(6000);
  await toast.waitFor({ state: "detached" });
});

test("Spam teaches the filter and the toast says what happened", async () => {
  const page = await mock.page();
  await page.goto(`${mock.origin}/`);

  await row(page, "Sample message 146").getByRole("button", { name: "Spam" }).click();

  await page
    .locator("#toasts .alert-success")
    .getByText("learned as spam, moved to Junk")
    .waitFor();
});

test("a button keeps its width, with a spinner in place of its label, while its request runs", async () => {
  const page = await mock.page();
  await page.route("**/decisions/*/undo", async (route) => {
    await new Promise((resolve) => setTimeout(resolve, 800));
    await route.continue();
  });
  await page.goto(`${mock.origin}/`);
  const button = row(page, "Sample message 142").getByRole("button", { name: /Undo|Working/ });

  const width = (await button.boundingBox()).width;

  await button.click();

  await button.getByText("Working…").waitFor({ state: "attached" });
  await button.locator(".loading-spinner").waitFor();
  assert.equal((await button.boundingBox()).width, width);
});

test("the Refresh button shows a spinner and Working… while the list reloads", async () => {
  const page = await mock.page();
  await page.goto(`${mock.origin}/`);
  await page.waitForSelector("tbody tr[data-state]");
  await page.route(/\/\?/, async (route) => {
    await new Promise((resolve) => setTimeout(resolve, 800));
    await route.continue();
  });
  const refresh = page.locator("#toolbar button[type=submit]");

  await refresh.click();

  await refresh.getByText("Working…").waitFor({ state: "attached" });
  await refresh.locator(".loading-spinner").waitFor();
  await refresh.getByText("Refresh", { exact: true }).waitFor();
});

test("the toast region stays on top of an open modal", async () => {
  const page = await mock.page();
  await page.goto(`${mock.origin}/whitelist`);
  await page.getByRole("checkbox", { name: /Select person001@/ }).check();
  await page.getByRole("button", { name: "Remove selected…" }).click();
  await page.locator("dialog[open]").waitFor();

  const top = await page.evaluate(() =>
    document.getElementById("toasts").matches("[popover]"),
  );
  assert.equal(top, true);
});
