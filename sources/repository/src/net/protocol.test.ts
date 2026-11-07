import { describe, expect, test } from "vite-plus/test";
import {
  capabilitiesSupports,
  parseCapabilities,
  RefPrefixes,
  refPrefixesFromSpecs,
  RefSpecOperation,
  isHttpProtocol,
  isHttpsProtocol,
  protocolPort,
} from "../main.ts";

describe("Protocol (src/net/protocol.rs)", () => {
  test("new() starts empty and intoArgs() yields nothing", () => {
    const p = new RefPrefixes();
    expect(p.intoArgs()).toEqual([]);
  });

  test("protocol helpers verify scheme and default ports", () => {
    expect(isHttpProtocol("http")).toBe(true);
    expect(isHttpProtocol("https")).toBe(false);
    expect(isHttpsProtocol("https")).toBe(true);
    expect(isHttpsProtocol("http")).toBe(false);
    expect(protocolPort("http")).toBe(80);
    expect(protocolPort("https")).toBe(443);
  });

  test("parseCapabilities() splits the advertised capability line", () => {
    const caps = parseCapabilities("agent=git/2.34.1 ls-refs=shallow fetch=wait-for-done");
    expect(caps).toContain("agent");
    expect(caps).toContain("ls-refs");
    expect(caps).toContain("fetch");
  });

  test("capabilitiesSupports() reports present, negated, and absent features", () => {
    const line = "agent=git/2.34.1 fetch=wait-for-done -unborn";
    expect(capabilitiesSupports(line, "fetch")).toBe(true);
    expect(capabilitiesSupports(line, "unborn")).toBe(false);
    expect(capabilitiesSupports(line, "ref-in-want")).toBeNull();
  });
});
