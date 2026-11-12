import assert from "node:assert/strict";
import { after, before, test } from "node:test";
import { startMock } from "./harness.mjs";

let mock;
before(async () => {
  mock = await startMock({ rows: 20, fail: true });
});
after(() => mock?.stop());

test("a failed action raises an error toast that stays until closed", async () => {
  const page = await mock.page();
  await page.clock.install();
  await page.goto(`${mock.origin}/`);

  await page.locator("tbody tr button").first().click();

  const toast = page.locator("#toasts .alert-error");
  await toast.getByText(/mailbox moved on/).waitFor();
  // Past the 5 s a success toast lasts: an error toast stays.
  await page.clock.fastForward(10_000);
  assert.equal(await toast.count(), 1);
  await toast.getByRole("button", { name: "Close" }).click();
  await toast.waitFor({ state: "detached" });
});

test("an expired session tells the user to reload", async () => {
  const page = await mock.page();
  await page.goto(`${mock.origin}/`);
  await page.route("**/decisions/*/undo", (route) => route.fulfill({ status: 401 }));
  const undo = page.locator("tbody tr").getByRole("button", { name: "Undo" }).first();

  await undo.click();

  await page
    .locator("#toasts .alert-error")
    .getByText(/Not signed in or the connection dropped/)
    .waitFor();
});

test("the health page reports the daemon's last error", async () => {
  const page = await mock.page();
  await page.goto(`${mock.origin}/health`);

  await page.getByText("IMAP login rejected").waitFor();
  await page.locator(".alert-error").waitFor();
});
