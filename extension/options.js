// Pairing lives here rather than in the worker: it is the one exchange a person
// drives, and this page is the only part of the extension they ever see.
//
// The code the app shows is the shared secret for exactly one exchange. This
// side proves it knows the code first - six digits are guessable from a proof,
// so BentoLaunch must not answer one it was not asked for - and then checks
// BentoLaunch's own proof before storing the token it sends back.

const PAIR_TIMEOUT_MS = 10000;

const status = document.getElementById("status");
const message = document.getElementById("message");
const code = document.getElementById("code");
const port = document.getElementById("port");
const pairing = document.getElementById("pairing");
const unpair = document.getElementById("unpair");

async function settings() {
  const stored = await chrome.storage.local.get(["port", "token"]);
  return { port: stored.port || 8777, token: stored.token || "" };
}

async function render() {
  const stored = await settings();
  port.value = stored.port;
  const paired = !!stored.token;

  status.textContent = paired
    ? "Paired with BentoLaunch. Open tabs appear in the panel automatically."
    : "Not paired yet.";
  status.className = paired ? "paired" : "";
  pairing.hidden = paired;
  unpair.hidden = !paired;
}

function say(text, good) {
  message.textContent = text;
  message.className = good ? "good" : "bad";
}

// One socket, one attempt. BentoLaunch closes its pairing window on a wrong code,
// so retrying means asking it for a new one.
function pair(digits, portNumber) {
  return new Promise((resolve, reject) => {
    let settled = false;
    const finish = (fn, value) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      try {
        socket.close();
      } catch (e) {
        // Already closing.
      }
      fn(value);
    };

    const socket = new WebSocket(`ws://127.0.0.1:${portNumber}/`);
    const timer = setTimeout(
      () => finish(reject, new Error("BentoLaunch did not answer. Is the pairing window open?")),
      PAIR_TIMEOUT_MS,
    );
    const nonce = randomHex(16);

    socket.onopen = async () => {
      socket.send(
        JSON.stringify({
          type: "hello",
          v: BRIDGE_PROTOCOL,
          mode: "pair",
          nonce,
          proof: await bridgeProof("pair-client", digits, nonce, ""),
        }),
      );
    };

    socket.onmessage = async (event) => {
      let reply;
      try {
        reply = JSON.parse(event.data);
      } catch (e) {
        return;
      }
      if (reply.type === "outdated") {
        finish(
          reject,
          new Error(
            `BentoLaunch speaks bridge protocol ${reply.protocol}, this extension speaks ` +
              `${BRIDGE_PROTOCOL}. Update ${outdatedSide(reply.protocol)} and try again.`,
          ),
        );
        return;
      }
      if (reply.type !== "paired") return;

      // What tells us the token came from the app that showed the code, rather
      // than from whatever happened to answer the port.
      const expected = await bridgeProof("pair-server", digits, nonce, "");
      if (reply.proof !== expected) {
        finish(reject, new Error("That was not BentoLaunch. Nothing has been paired."));
        return;
      }
      finish(resolve, reply.token);
    };

    // A refused or dropped socket is what a wrong code looks like from here:
    // BentoLaunch answers a wrong proof with silence.
    socket.onclose = () =>
      finish(reject, new Error("BentoLaunch refused the code. Ask it for a new one and try again."));
    socket.onerror = () =>
      finish(reject, new Error("Could not reach BentoLaunch on that port. Is it running?"));
  });
}

document.getElementById("pair").addEventListener("click", async () => {
  const digits = code.value.replace(/\D/g, "");
  if (digits.length !== 6) {
    say("Enter the six digits BentoLaunch is showing.", false);
    return;
  }

  const portNumber = Number(port.value) || 8777;
  say("Pairing…", true);
  try {
    const token = await pair(digits, portNumber);
    await chrome.storage.local.set({ port: portNumber, token });
    code.value = "";
    say("", true);
    await render();
  } catch (e) {
    say(e.message, false);
  }
});

document.getElementById("forget").addEventListener("click", async () => {
  await chrome.storage.local.remove("token");
  say("", true);
  await render();
});

port.addEventListener("change", async () => {
  await chrome.storage.local.set({ port: Number(port.value) || 8777 });
});

render();
