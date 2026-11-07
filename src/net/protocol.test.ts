import { describe, expect, it } from "vite-plus/test";
import {
  RefPrefixes,
  refPrefixesFromSpecs,
  executeGitProtocolRequest,
  gitServiceUrl,
  type ProtocolCommand,
} from "../main.ts";

describe("Protocol & Service URL (src/net/protocol.rs)", () => {
  it("gitServiceUrl returns smart HTTP endpoints", () => {
    expect(gitServiceUrl("https://github.com/repo", 0 as ProtocolCommand)).toBe(
      "https://github.com/repo/info/refs?service=git-upload-pack",
    );
    expect(gitServiceUrl("https://github.com/repo/", 1 as ProtocolCommand)).toBe(
      "https://github.com/repo/git-upload-pack",
    );
  });

  it("RefPrefixes works as expected", () => {
    const p = new RefPrefixes();
    p.extend(["refs/heads/", "HEAD"]);
    const args = p.intoArgs();
    expect(args).toContain("ref-prefix refs/heads/");
    expect(args).toContain("ref-prefix HEAD");
  });

  it("refPrefixesFromSpecs works as expected", () => {
    const prefixes = refPrefixesFromSpecs(["refs/heads/*:refs/heads/*", "HEAD"]);
    expect(prefixes).toContain("refs/heads/");
    expect(prefixes).toContain("HEAD");
  });

  it("executeGitProtocolRequest fails gracefully when endpoint is unreachable", () => {
    expect(() =>
      executeGitProtocolRequest("http://127.0.0.1:59999/repo", 0 as ProtocolCommand),
    ).toThrow();
  });
});
