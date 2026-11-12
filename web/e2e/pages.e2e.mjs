import assert from "node:assert/strict";
import { after, before, test } from "node:test";
import { startMock } from "./harness.mjs";

let mock;
before(async () => {
  mock = await startMock({ rows: 20 });
});
after(() => mock?.stop());

test("the navigation links between pages and marks the current one", async () => {
  const page = await mock.page();
  await page.goto(`${mock.origin}/`);
  const nav = page.getByRole("navigation", { name: "Main" });

  for (const name of ["Health", "Whitelist", "Rules", "Logs", "Decisions"]) {
    await nav.getByRole("link", { name }).click();
    await page.getByRole("heading", { level: 1, name }).waitFor();
    assert.equal(
      await nav.getByRole("link", { name }).getAttribute("aria-current"),
      "page",
    );
  }
});

test("the health page lists the daemon and its checks, and Refresh keeps them", async () => {
  const page = await mock.page();
  await page.goto(`${mock.origin}/health`);

  await page.getByText("Uptime").waitFor();
  await page.getByText("Last pass").waitFor();
  const badges = await page.locator("tbody .badge").count();
  assert.ok(badges > 0);

  await page.getByRole("button", { name: "Refresh" }).click();
  await page.getByText("Uptime").waitFor();
  assert.equal(await page.locator("tbody .badge").count(), badges);
});

test("the logs page shows the lines and turns debug logging on and off", async () => {
  const page = await mock.page();
  await page.goto(`${mock.origin}/logs`);

  await page.getByText("pass started: INBOX, Junk").waitFor();
  await page.locator(".badge-error", { hasText: "ERROR" }).waitFor();
  await page.getByText("Debug logging off").waitFor();

  await page.getByRole("button", { name: /Turn on for/ }).click();
  await page.getByText(/Debug logging on until/).waitFor();
  await page.getByRole("button", { name: "Turn off" }).click();
  await page.getByText("Debug logging off").waitFor();
});

test("the rules page colours the script and says where a message would go", async () => {
  const page = await mock.page();
  await page.goto(`${mock.origin}/rules`);
  await page.locator("#rules-source span.text-primary").first().waitFor();
  await page.getByText("Valid", { exact: true }).waitFor();
  const message = page.getByRole("textbox", { name: "Message to try" });
  const result = page.locator("#result");

  await message.fill("From: x@y.org\nList-Id: <announce.example.org>\n\nhello");
  await page.getByRole("button", { name: "Try" }).click();
  await result.getByText("Newsletters").waitFor();

  await message.fill("From: billing@acme.example.com\n\nreceipt");
  await page.getByRole("button", { name: "Try" }).click();
  await result.getByText("Receipts").waitFor();

  await message.fill("From: friend@example.org\n\nhi");
  await page.getByRole("button", { name: "Try" }).click();
  await result.getByText("Stays in the inbox").waitFor();
});

test("the sign-in page offers a passkey button and no navigation", async () => {
  const page = await mock.page();
  await page.goto(`${mock.origin}/auth/login`);

  await page.getByRole("button", { name: "Sign in with a passkey" }).waitFor();
  assert.equal(await page.getByRole("navigation", { name: "Main" }).count(), 0);
});

test("the pages fit a phone without scrolling sideways", async () => {
  const page = await mock.page({ viewport: { width: 390, height: 800 } });
  for (const path of ["/health", "/logs", "/rules", "/auth/login"]) {
    await page.goto(`${mock.origin}${path}`);
    const overflow = await page.evaluate(
      () => document.documentElement.scrollWidth - window.innerWidth,
    );
    assert.ok(overflow <= 0, `${path} overflows by ${overflow}px`);
  }
});
