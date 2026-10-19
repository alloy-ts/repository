const test = require("node:test");
const assert = require("node:assert");

test("CJS loader smoke test for repository binding", async () => {
  const { Repository } = await import("./dist/index.js");
  assert.strictEqual(typeof Repository.open, "function");
  assert.strictEqual(typeof Repository.init, "function");
  assert.strictEqual(typeof Repository.discover, "function");
  assert.strictEqual(typeof Repository.clone, "function");
});
