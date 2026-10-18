import { test } from "node:test";
import assert from "node:assert/strict";
import { add } from "../dist/index.js";

test("add(1, 2) returns 3", () => {
  assert.equal(add(1, 2), 3);
});
