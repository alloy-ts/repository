const assert = require("node:assert");
const { Repository } = require("./dist/index.js");

const repo = Repository.open(".");
assert.strictEqual(typeof repo.headId(), "string");
assert.ok(repo.headId().length > 0);
