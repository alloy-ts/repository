import assert from "node:assert/strict";

const origin = process.argv[2] ?? "http://127.0.0.1:8080";
const health = await fetch(new URL("/health", origin));
assert.equal(health.status, 200);
assert.deepEqual(await health.json(), { ok: true });
const echo = await fetch(new URL("/echo", origin), {
  method: "POST",
  body: new Uint8Array([0, 255, 1]),
});
assert.equal(echo.status, 200);
assert.deepEqual(new Uint8Array(await echo.arrayBuffer()), new Uint8Array([0, 255, 1]));
const empty = await fetch(new URL("/echo", origin), { method: "POST" });
assert.equal(empty.status, 200);
assert.equal((await empty.arrayBuffer()).byteLength, 0);
const head = await fetch(new URL("/health", origin), { method: "HEAD" });
assert.equal(head.status, 200);
assert.equal(await head.text(), "");
assert.equal((await fetch(new URL("/missing", origin))).status, 404);
console.log(`${origin}: health, binary echo, empty body, HEAD and 404 passed`);
