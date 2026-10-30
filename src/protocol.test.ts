import { describe, expect, test } from "vite-plus/test";

import {
  capabilitiesSupports,
  parseCapabilities,
  RefPrefixes,
  refPrefixesFromSpecs,
  RefSpecOperation,
} from "./main.ts";

describe("RefPrefixes (src/protocol.rs)", () => {
  test("new() starts empty and intoArgs() yields nothing", () => {
    const p = new RefPrefixes();
    expect(p.intoArgs()).toEqual([]);
  });

  test("fromRefspecs() expands a glob into a two-component prefix", () => {
    const p = RefPrefixes.fromRefspecs(["refs/heads/*:refs/heads/*"]);
    const args = p.intoArgs();
    expect(args).toContain("ref-prefix refs/heads/");
    // Exactly one prefix for a single bare glob side.
    expect(args.filter((a) => a === "ref-prefix refs/heads/").length).toBe(1);
  });

  test("fromRefspecs() expands an exact ref and DWIM-style loose names", () => {
    const p = RefPrefixes.fromRefspecs(["HEAD", "main", "refs/heads/main"]);
    const wired = p.intoArgs().map((a) => a.replace(/^ref-prefix /, ""));
    // Exact refs are kept verbatim, loose names are expanded DWIM-style.
    expect(wired).toContain("HEAD");
    expect(wired).toContain("refs/heads/main");
    expect(wired).toContain("refs/remotes/main");
    expect(wired).toContain("refs/tags/main");
  });

  test("fromRefspecs() honors an explicit RefSpecOperation", () => {
    const fetch = RefPrefixes.fromRefspecs(["refs/heads/*:refs/heads/*"], RefSpecOperation.Fetch);
    const push = RefPrefixes.fromRefspecs(["refs/heads/*:refs/heads/*"], RefSpecOperation.Push);
    // The prefix set is identical for a simple glob regardless of operation; the
    // call must simply complete and produce the same wire form.
    expect(fetch.intoArgs()).toEqual(push.intoArgs());
  });

  test("extend() preserves first-seen order and de-duplicates", () => {
    const p = new RefPrefixes();
    p.extend(["refs/heads/", "HEAD", "refs/heads/"]);
    const args = p.intoArgs();
    expect(args[0]).toBe("ref-prefix refs/heads/");
    expect(args).toContain("ref-prefix HEAD");
    expect(args.filter((a) => a === "ref-prefix refs/heads/").length).toBe(1);
    expect(args.filter((a) => a === "ref-prefix HEAD").length).toBe(1);
  });
});

describe("refPrefixesFromSpecs (src/protocol.rs)", () => {
  test("returns the bare prefixes (no `ref-prefix ` wire prefix)", () => {
    const bare = refPrefixesFromSpecs(["refs/heads/*:refs/heads/*", "HEAD"]);
    expect(bare).toContain("refs/heads/");
    expect(bare).toContain("HEAD");
    // No entry is wrapped in the wire form.
    expect(bare.some((p) => p.startsWith("ref-prefix "))).toBe(false);
  });

  test("expands DWIM-style loose refs the same as gix", () => {
    const bare = refPrefixesFromSpecs(["main", "refs/heads/main"]);
    expect(bare).toContain("refs/heads/main");
    expect(bare).toContain("refs/remotes/main");
    expect(bare).toContain("refs/tags/main");
  });

  test("defaults to Fetch when no operation is supplied", () => {
    const withDefault = refPrefixesFromSpecs(["refs/heads/*:refs/heads/*"]);
    const withFetch = refPrefixesFromSpecs(["refs/heads/*:refs/heads/*"], RefSpecOperation.Fetch);
    expect(withDefault).toEqual(withFetch);
  });

  test("prefixes match RefPrefixes.intoArgs() wire form", () => {
    const specs = ["refs/heads/*:refs/heads/*", "HEAD", "main"];
    const bare = refPrefixesFromSpecs(specs).slice().sort();
    const wired = RefPrefixes.fromRefspecs(specs)
      .intoArgs()
      .map((a) => a.replace(/^ref-prefix /, ""))
      .sort();
    expect(wired).toEqual(bare);
  });
});

describe("Capabilities (src/protocol.rs)", () => {
  test("parseCapabilities() splits the advertised capability line", () => {
    const caps = parseCapabilities("agent=git/2.34.1 ls-refs=shallow fetch=wait-for-done");
    expect(caps).toContain("agent");
    expect(caps).toContain("ls-refs");
    expect(caps).toContain("fetch");
  });

  test("capabilitiesSupports() reports present, negated, and absent features", () => {
    // `unborn` is advertised as negated here; `ref-in-want` is absent entirely.
    const line = "agent=git/2.34.1 fetch=wait-for-done -unborn";
    expect(capabilitiesSupports(line, "fetch")).toBe(true);
    expect(capabilitiesSupports(line, "unborn")).toBe(false);
    expect(capabilitiesSupports(line, "ref-in-want")).toBeNull();
  });
});

describe("Transport and Protocol execution (src/protocol.rs)", () => {
  test("executeProtocolHttpRequest handles HTTP request errors gracefully", async () => {
    const { executeProtocolHttpRequest } = await import("./main.ts");
    const res = await executeProtocolHttpRequest("http://127.0.0.1:59998/test", "GET");
    expect(res).toBeDefined();
    expect(typeof res.status).toBe("number");
  });

  test("executeProtocolSshCommand handles SSH command execution", async () => {
    const { executeProtocolSshCommand } = await import("./main.ts");
    const res = await executeProtocolSshCommand(
      "127.0.0.1",
      "git-upload-pack /repo",
      "invalid-user",
    );
    expect(res).toBeDefined();
    expect(typeof res.status).toBe("number");
  });
});
