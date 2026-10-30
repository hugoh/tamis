const status = document.getElementById("status");

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
  document.getElementById(id)?.addEventListener("click", async () => {
    try {
      await ceremony();
      location.href = "/";
    } catch (err) {
      status.textContent = err.message;
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
  const token = new URLSearchParams(location.search).get("token") ?? "";
  const options = await (
    await send("/auth/register/begin?token=" + encodeURIComponent(token))
  ).json();
  const credential = await navigator.credentials.create({
    publicKey: PublicKeyCredential.parseCreationOptionsFromJSON(
      options.publicKey,
    ),
  });
  const name = document.getElementById("name").value;
  await send(
    "/auth/register/finish?name=" + encodeURIComponent(name),
    JSON.stringify(credential.toJSON()),
  );
});
