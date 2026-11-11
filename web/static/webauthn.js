const status = document.getElementById("status");

// Passkeys need the WebAuthn JSON helpers and a secure context. Anything else is
// a definitive "no", so say so up front instead of failing on click. (The
// capability probe getClientCapabilities() is left out on purpose: it reports
// no platform authenticator for people who sign in with a security key.)
const unsupported = () =>
  globalThis.isSecureContext === false ||
  typeof PublicKeyCredential === "undefined" ||
  typeof PublicKeyCredential.parseRequestOptionsFromJSON !== "function"
    ? "Passkeys are not available here: they need a current browser and an HTTPS connection."
    : "";

// NotAllowedError covers a dismissed prompt, a timeout and a missing
// credential alike, so word it neutrally instead of showing the browser's text.
const describe = (err) =>
  err?.name === "NotAllowedError"
    ? "Cancelled or timed out. Try again."
    : err.message;

async function send(url, body) {
  const res = await fetch(url, {
    method: "POST",
    credentials: "same-origin",
    headers: { "Content-Type": "application/json" },
    body,
  });
  if (!res.ok) throw new Error((await res.text()).trim() || res.statusText);
  return res;
}

function on(id, ceremony) {
  const button = document.getElementById(id);
  if (!button) return;
  const problem = unsupported();
  if (problem) {
    status.textContent = problem;
    button.disabled = true;
    return;
  }
  button.addEventListener("click", async () => {
    status.textContent = "";
    button.disabled = true;
    button.setAttribute("aria-busy", "true");
    try {
      await ceremony();
      location.href = "/";
    } catch (err) {
      status.textContent = describe(err);
    } finally {
      button.disabled = false;
      button.removeAttribute("aria-busy");
    }
  });
}

on("login", async () => {
  const options = await (await send("/auth/login/begin")).json();
  const credential = await navigator.credentials.get({
    publicKey: PublicKeyCredential.parseRequestOptionsFromJSON(
      options.publicKey,
    ),
  });
  await send("/auth/login/finish", JSON.stringify(credential.toJSON()));
});

on("enroll", async () => {
  const token = new URLSearchParams(location.hash.slice(1)).get("token") ?? "";
  const options = await (
    await send("/auth/register/begin", JSON.stringify({ token }))
  ).json();
  const credential = await navigator.credentials.create({
    publicKey: PublicKeyCredential.parseCreationOptionsFromJSON(
      options.publicKey,
    ),
  });
  const name = document.getElementById("name").value;
  await send(
    `/auth/register/finish?name=${encodeURIComponent(name)}`,
    JSON.stringify(credential.toJSON()),
  );
});
