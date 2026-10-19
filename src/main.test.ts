import { expect, test } from "vite-plus/test";
import { add } from "../dist/index.js";

test("add returns sum of two numbers", () => {
  expect(add(1, 2)).toBe(3);
});
