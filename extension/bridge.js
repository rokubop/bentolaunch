// The hash both sides of the bridge compute, and nothing else.
//
// Shared by the worker and the options page: pairing happens in the options
// page, resuming happens in the worker, and the two exchanges have to agree
// with `src/browser/crypto.rs` down to the separator or nothing ever proves
// itself.

// What this build of the bridge speaks. Must match PROTOCOL in
// `src/browser/protocol.rs` - the exe and this extension are separate
// downloads, so the two drift and the mismatch has to be explainable.
const BRIDGE_PROTOCOL = 3;

// Which half to tell the user to update. `theirs` is what BentoLaunch reports.
function outdatedSide(theirs) {
  return theirs > BRIDGE_PROTOCOL ? "this extension" : "BentoLaunch";
}

const HEX = (bytes) => [...bytes].map((b) => b.toString(16).padStart(2, "0")).join("");

function randomHex(byteCount) {
  const buffer = new Uint8Array(byteCount);
  crypto.getRandomValues(buffer);
  return HEX(buffer);
}

async function sha256Hex(text) {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
  return HEX(new Uint8Array(digest));
}

// Every field is hex or digits, so NUL separators make the concatenation
// unambiguous. The label is what stops a proof being replayed back the other
// way: each side hashes the same secret and nonces under a different name.
async function bridgeProof(label, secret, nonceClient, nonceServer) {
  return sha256Hex(`bentolaunch\0${label}\0${secret}\0${nonceClient}\0${nonceServer}`);
}
