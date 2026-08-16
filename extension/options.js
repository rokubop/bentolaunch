const port = document.getElementById("port");
const token = document.getElementById("token");
const saved = document.getElementById("saved");

document.getElementById("origin").textContent = `chrome-extension://${chrome.runtime.id}`;

chrome.storage.local.get(["port", "token"]).then((stored) => {
  port.value = stored.port || 8777;
  token.value = stored.token || "";
});

document.getElementById("save").addEventListener("click", async () => {
  await chrome.storage.local.set({
    port: Number(port.value) || 8777,
    token: token.value.trim(),
  });
  saved.textContent = "saved";
  setTimeout(() => (saved.textContent = ""), 1500);
});
