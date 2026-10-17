import { expect, test } from "vite-plus/test";
import { main } from "./main.ts";

test("main returns Hello, world!", () => {
  expect(main()).toBe("Hello, world!");
});
