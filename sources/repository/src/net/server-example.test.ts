import { expect, test } from "vite-plus/test";
import { createApp, demonstrate } from "../../../../examples/server.ts";

test("examples/server.ts exercises the public server API in process", async () => {
  const lines: string[] = [];
  const original = console.log;
  console.log = (...args: unknown[]) => {
    lines.push(args.map(String).join(" "));
  };
  try {
    await demonstrate(createApp(0));
  } finally {
    console.log = original;
  }
  expect(lines.join("\n")).toContain("GET /ping 200 pong 1");
  expect(lines.join("\n")).toContain("QUERY /search 200 found");
});
