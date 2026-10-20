import { expect, test } from "vite-plus/test";
import { add } from "../dist/index.js";

test("add(1, 2) returns 3", () => {
  expect(add(1, 2)).toBe(3);
});
