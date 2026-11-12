// Starts the mock web interface (src/web/tests/mock.rs, `serve_mock`) on a free
// port and drives it with Playwright. Each test file starts its own server, so
// the actions one file takes (undo, remove) never leak into another.
import { spawn } from "node:child_process";
import { createRequire } from "node:module";
import { createServer } from "node:net";

// Playwright is installed by mise outside the repo; NODE_PATH points at it,
// which `require` honours and `import` does not.
const { chromium } = createRequire(import.meta.url)("playwright");

const freePort = () =>
  new Promise((resolve, reject) => {
    const server = createServer();
    server.once("error", reject);
    server.listen(0, () => {
      const { port } = server.address();
      server.close(() => resolve(port));
    });
  });

const ready = async (url, child, timeoutMs) => {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (child.exitCode !== null) throw new Error("the mock server exited");
    try {
      if ((await fetch(url)).ok) return;
    } catch {
      // not listening yet
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  throw new Error(`the mock server did not answer at ${url}`);
};

/** Serves sample data; `fail` makes every action and the daemon fail. */
export async function startMock({ rows = 150, fail = false } = {}) {
  const port = await freePort();
  // `mise run test:e2e` builds the test binary once and passes its path, so
  // the files run in parallel without contending for cargo's build lock.
  const [command, args] = process.env.TAMIS_MOCK_BIN
    ? [process.env.TAMIS_MOCK_BIN, ["serve_mock", "--ignored", "--nocapture"]]
    : ["cargo", ["test", "--lib", "serve_mock", "--", "--ignored", "--nocapture"]];
  const child = spawn(
    command,
    args,
    {
      env: {
        ...process.env,
        TAMIS_MOCK_PORT: String(port),
        TAMIS_MOCK_ROWS: String(rows),
        ...(fail && { TAMIS_MOCK_FAIL: "1" }),
      },
      detached: true,
      stdio: ["ignore", "ignore", "inherit"],
    },
  );
  const origin = `http://localhost:${port}`;
  try {
    await ready(`${origin}/health`, child, 300_000);
  } catch (error) {
    process.kill(-child.pid);
    throw error;
  }
  // CI uses the Chrome the runner already has (TAMIS_E2E_CHANNEL=chrome)
  // instead of downloading a browser.
  const browser = await chromium.launch(
    process.env.TAMIS_E2E_CHANNEL ? { channel: process.env.TAMIS_E2E_CHANNEL } : {},
  );
  return {
    origin,
    /** A fresh page on its own browser context. */
    async page(options = {}) {
      const context = await browser.newContext({
        viewport: { width: 1400, height: 900 },
        ...options,
      });
      context.setDefaultTimeout(10_000);
      return context.newPage();
    },
    async stop() {
      await browser.close();
      // cargo runs the test binary as a child: signal the whole group.
      process.kill(-child.pid);
    },
  };
}
