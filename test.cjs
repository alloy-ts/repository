const assert = require("node:assert");
const { test } = require("node:test");
const { Repository } = require("./dist/index.js");

test("smoke test from test.cjs", () => {
  assert.ok(Repository);
  assert.strictEqual(typeof Repository.open, "function");
  assert.strictEqual(typeof Repository.init, "function");
  assert.strictEqual(typeof Repository.discover, "function");
  assert.strictEqual(typeof Repository.clone, "function");
});
