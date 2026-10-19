const test = require('node:test');
const assert = require('node:assert');
const { Repository, add } = require('./dist/index.js');

test('CJS module exports Repository and add', () => {
  assert.strictEqual(typeof Repository, 'function');
  assert.strictEqual(add(2, 3), 5);
  const repo = Repository.open(process.cwd());
  assert.ok(typeof repo.headId() === 'string');
});
