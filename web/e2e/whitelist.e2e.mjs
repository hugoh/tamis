import assert from "node:assert/strict";
import { after, before, test } from "node:test";
import { startMock } from "./harness.mjs";

let mock;
before(async () => {
  mock = await startMock({ rows: 150 });
});
after(() => mock?.stop());

const search = (page, text) =>
  page.getByRole("searchbox", { name: "Search" }).fill(text);

test("the list shows a page of addresses and loads the rest on scroll", async () => {
  const page = await mock.page();
  await page.goto(`${mock.origin}/whitelist`);
  await page.getByText("150 addresses").waitFor();
  const boxes = page.locator("tbody input[name=address]");
  assert.equal(await boxes.count(), 100);

  await page.locator("tbody tr").last().scrollIntoViewIfNeeded();

  await page.waitForFunction(
    () => document.querySelectorAll("tbody input[name=address]").length === 150,
  );
});

test("searching narrows the addresses", async () => {
  const page = await mock.page();
  await page.goto(`${mock.origin}/whitelist`);

  await search(page, "person007");

  await page.getByText("1 address").waitFor();
  assert.equal(await page.locator("tbody input[name=address]").count(), 1);
});

test("adding an address lists it and clears the form", async () => {
  const page = await mock.page();
  await page.goto(`${mock.origin}/whitelist`);
  const input = page.getByRole("textbox", { name: "Address to whitelist" });

  await input.fill("newcomer@example.org");
  await page.getByRole("button", { name: "Add", exact: true }).click();

  await search(page, "newcomer");
  await page.getByRole("cell", { name: "newcomer@example.org", exact: true }).waitFor();
  assert.equal(await input.inputValue(), "");
});

test("Select all ticks every loaded address", async () => {
  const page = await mock.page();
  await page.goto(`${mock.origin}/whitelist`);

  await page.locator("#select-all").check();

  const unchecked = await page
    .locator("tbody input[name=address]:not(:checked)")
    .count();
  assert.equal(unchecked, 0);
});

test("removing asks first: Cancel keeps the addresses, Remove deletes them", async () => {
  const page = await mock.page();
  await page.goto(`${mock.origin}/whitelist`);
  await search(page, "person01");
  await page.getByText("10 addresses").waitFor();
  const pick = (n) => page.getByRole("checkbox", { name: `Select person01${n}@example.org` });
  await pick(1).check();
  await pick(2).check();

  await page.getByRole("button", { name: "Remove selected…" }).click();
  const dialog = page.locator("dialog[open]");
  await dialog.getByText("person011@example.org").waitFor();
  await dialog.getByRole("button", { name: "Cancel" }).click();
  await dialog.waitFor({ state: "detached" });
  assert.equal(await page.locator("tbody input[name=address]").count(), 10);

  await page.getByRole("button", { name: "Remove selected…" }).click();
  await page.locator("dialog[open]").getByRole("button", { name: /^Remove 2/ }).click();

  await page.getByText("8 addresses").waitFor();
  assert.equal(await page.getByRole("cell", { name: "person011@example.org" }).count(), 0);
});

test("clicking outside the dialog closes it", async () => {
  const page = await mock.page();
  await page.goto(`${mock.origin}/whitelist`);
  await page.locator("tbody input[name=address]").first().check();
  await page.getByRole("button", { name: "Remove selected…" }).click();
  const dialog = page.locator("dialog[open]");
  await dialog.waitFor();

  await page.mouse.click(8, 8);

  await dialog.waitFor({ state: "detached" });
  assert.equal(await page.locator("tbody input[name=address]").count() > 0, true);
});

test("Escape closes the dialog too", async () => {
  const page = await mock.page();
  await page.goto(`${mock.origin}/whitelist`);
  await page.locator("tbody input[name=address]").first().check();
  await page.getByRole("button", { name: "Remove selected…" }).click();
  const dialog = page.locator("dialog[open]");
  await dialog.waitFor();

  await page.keyboard.press("Escape");

  await dialog.waitFor({ state: "detached" });
});
