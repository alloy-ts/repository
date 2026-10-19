const assert = require("node:assert");
const test = require("node:test");
const { Repository } = require("./dist/index.js");

void test("Repository basic node test", () => {
  const repo = Repository.open(process.cwd());
  assert.strictEqual(typeof repo.gitDir(), "string");
  assert.ok(repo.gitDir().length > 0);
});
