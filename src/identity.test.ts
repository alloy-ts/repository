import { beforeAll, describe, expect, test } from "vite-plus/test";
import {
  identityToRaw,
  parseIdentity,
  parseSignature,
  Repository,
  signatureToIdentity,
  signatureToRaw,
} from "./main.ts";

let repo: Repository;

beforeAll(() => {
  // The bindings only need a repository to operate on; the project itself is one.
  repo = Repository.open(process.cwd());
});

describe("identity parsing", () => {
  test("parseIdentity() parses, trims, and returns null on garbage", () => {
    const actor = parseIdentity(" Taylor Example < taylor@example.com >");
    expect(actor).toEqual({
      name: "Taylor Example",
      email: "taylor@example.com",
    });

    // Whitespace inside the name is preserved; only the edges are trimmed.
    expect(parseIdentity("Jane Q. Public <jane@example.com> 1700000000 +0000")).toEqual({
      name: "Jane Q. Public",
      email: "jane@example.com",
    });

    expect(parseIdentity("no angle brackets here")).toBeNull();
    expect(parseIdentity("Jane <never-closed")).toBeNull();
    expect(parseIdentity("")).toBeNull();
  });

  test("parseSignature() decodes the timestamp or returns null for malformed input", () => {
    const sig = parseSignature("Taylor Example <taylor@example.com> 1711398853 +0800");
    expect(sig).toEqual({
      name: "Taylor Example",
      email: "taylor@example.com",
      time: 1711398853,
      timezoneOffset: 8 * 60 * 60,
    });

    // The timestamp is optional for parsing; the offset decodes to seconds.
    expect(parseSignature("Jane <jane@example.com> 1700000000 +0130")).toMatchObject({
      name: "Jane",
      email: "jane@example.com",
      time: 1700000000,
      timezoneOffset: 5400,
    });

    // A well-formed identity without a timestamp parses with zeroed time.
    expect(parseSignature("Jane <jane@example.com>")).toEqual({
      name: "Jane",
      email: "jane@example.com",
      time: 0,
      timezoneOffset: 0,
    });

    expect(parseSignature("no angle brackets here")).toBeNull();
    expect(parseSignature("")).toBeNull();
  });
});

describe("identity formatting", () => {
  test("identityToRaw() serialises `Name <email>`", () => {
    expect(identityToRaw({ name: "Taylor Example", email: "taylor@example.com" })).toBe(
      "Taylor Example <taylor@example.com>",
    );
  });

  test("signatureToRaw() serialises the full git actor format", () => {
    expect(
      signatureToRaw({
        name: "Taylor Example",
        email: "taylor@example.com",
        time: 1711398853,
        timezoneOffset: 8 * 60 * 60,
      }),
    ).toBe("Taylor Example <taylor@example.com> 1711398853 +0800");

    // Fractional-hour offsets round-trip through gix-date's native seconds.
    expect(
      signatureToRaw({
        name: "Jane",
        email: "jane@example.com",
        time: 1700000000,
        timezoneOffset: 5400,
      }),
    ).toBe("Jane <jane@example.com> 1700000000 +0130");
  });

  test("signatureToRaw() rejects names or emails containing git actor delimiters", () => {
    for (const bad of ["a<b", "a>b", "a\nb"]) {
      expect(() =>
        signatureToRaw({ name: bad, email: "x@y", time: 0, timezoneOffset: 0 }),
      ).toThrow();
      expect(() =>
        signatureToRaw({ name: "ok", email: bad, time: 0, timezoneOffset: 0 }),
      ).toThrow();
    }
  });
});

describe("signature/identity conversions", () => {
  test("signatureToIdentity() drops the timestamp, mirroring signature.actor()", () => {
    const sig = parseSignature("Taylor Example <taylor@example.com> 1711398853 +0800");
    expect(sig).not.toBeNull();
    expect(signatureToIdentity(sig!)).toEqual(parseIdentity("Taylor Example <taylor@example.com>"));

    // Also works on hand-built signatures without parsing.
    expect(
      signatureToIdentity({
        name: "J",
        email: "j@x",
        time: 42,
        timezoneOffset: 0,
      }),
    ).toEqual({
      name: "J",
      email: "j@x",
    });
  });

  test("parse/format round-trips", () => {
    const raw = "Taylor Example <taylor@example.com> 1711398853 +0800";
    const sig = parseSignature(raw);
    expect(sig).not.toBeNull();
    expect(signatureToRaw(sig!)).toBe(raw);

    const identityRaw = "Taylor Example <taylor@example.com>";
    expect(identityToRaw(parseIdentity(identityRaw)!)).toBe(identityRaw);
  });
});

describe("repository identities", () => {
  test("defaultAuthor()/defaultCommitter() reflect the configured identity", () => {
    const author = repo.defaultAuthor();
    expect(author).not.toBeNull();
    expect(typeof author!.name).toBe("string");
    expect(typeof author!.email).toBe("string");
    expect(typeof author!.time).toBe("number");
    expect(typeof author!.timezoneOffset).toBe("number");
  });

  test("committerOrSetFallback() returns the configured committer when one exists", () => {
    const fallback = repo.committerOrSetFallback("Example Actor", "actor@example.com");
    const configured = repo.defaultCommitter();
    if (configured) {
      // A committer is configured; the fallback is not installed.
      expect(fallback).toEqual(configured);
    } else {
      // Otherwise the in-memory fallback is installed and returned.
      expect(fallback.name).toBe("Example Actor");
      expect(fallback.email).toBe("actor@example.com");
      expect(fallback.time).toBeGreaterThan(0);
    }
  });

  test("committerOrSetGenericFallback() always returns a usable identity", () => {
    const fallback = repo.committerOrSetGenericFallback();
    expect(fallback.name.length).toBeGreaterThan(0);
    expect(fallback.email.length).toBeGreaterThan(0);
  });

  test("authorOrSetFallback() returns the configured author when one exists", () => {
    const fallback = repo.authorOrSetFallback("Example Author", "author@example.com");
    const configured = repo.defaultAuthor();
    if (configured) {
      // An author is configured; the fallback is not installed.
      expect(fallback).toEqual(configured);
    } else {
      // Otherwise the supplied fallback is returned.
      expect(fallback.name).toBe("Example Author");
      expect(fallback.email).toBe("author@example.com");
    }
  });

  test("authorOrSetGenericFallback() always returns a usable identity", () => {
    const fallback = repo.authorOrSetGenericFallback();
    expect(fallback.name.length).toBeGreaterThan(0);
    expect(fallback.email.length).toBeGreaterThan(0);
  });

  test("configured identities round-trip through the raw format", () => {
    const author = repo.defaultAuthor();
    expect(author).not.toBeNull();
    const parsed = parseSignature(signatureToRaw(author!));
    expect(parsed).toEqual(author);
  });
});
