import { describe, expect, test } from "vite-plus/test";

import { MatchGroup, RefSpec, RefSpecOperation } from "./main.ts";

// Deterministic 40-char hex object ids (sha1-style) for match items.
const sha = (n: number) => n.toString(16).padStart(40, "0");

const FETCH_PATTERN = "+refs/heads/*:refs/remotes/origin/*";

describe("RefSpec.parse", () => {
  test("parses a force fetch pattern and round-trips it", () => {
    const spec = RefSpec.parse(FETCH_PATTERN, RefSpecOperation.Fetch);
    expect(spec.spec).toBe(FETCH_PATTERN);
    expect(spec.operation).toBe(RefSpecOperation.Fetch);
    expect(spec.mode).toBe("force");
    expect(spec.allowNonFastForward()).toBe(true);
    expect(spec.source).toBe("refs/heads/*");
    expect(spec.destination).toBe("refs/remotes/origin/*");
    // For fetch specs the remote side is the source, the local side the destination.
    expect(spec.remote).toBe("refs/heads/*");
    expect(spec.local).toBe("refs/remotes/origin/*");

    const instr = spec.instruction();
    expect(instr.operation).toBe("fetch");
    expect(instr.kind).toBe("and_update");
    expect(instr.src).toBe("refs/heads/*");
    expect(instr.dst).toBe("refs/remotes/origin/*");
    expect(instr.allowNonFastForward).toBe(true);
  });

  test("parses a normal fetch pattern without force", () => {
    const spec = RefSpec.parse("refs/heads/*:refs/remotes/origin/*", RefSpecOperation.Fetch);
    expect(spec.mode).toBe("normal");
    expect(spec.allowNonFastForward()).toBe(false);
    expect(spec.instruction().kind).toBe("and_update");
    expect(spec.instruction().allowNonFastForward).toBe(false);
  });

  test("an empty fetch spec becomes a HEAD-only spec", () => {
    const spec = RefSpec.parse("", RefSpecOperation.Fetch);
    expect(spec.spec).toBe("HEAD");
    expect(spec.source).toBe("HEAD");
    expect(spec.destination).toBeNull();
    const instr = spec.instruction();
    expect(instr.kind).toBe("only");
    expect(instr.src).toBe("HEAD");
    expect(instr.dst).toBeUndefined();
  });

  test("the @ shorthand becomes HEAD", () => {
    const spec = RefSpec.parse("@", RefSpecOperation.Fetch);
    expect(spec.source).toBe("HEAD");
    expect(spec.instruction().kind).toBe("only");
  });

  test("parses push specs", () => {
    // Deletion: one-sided with only a destination.
    const del = RefSpec.parse(":refs/heads/feature", RefSpecOperation.Push);
    expect(del.operation).toBe(RefSpecOperation.Push);
    expect(del.mode).toBe("normal");
    expect(del.source).toBeNull();
    expect(del.destination).toBe("refs/heads/feature");
    expect(del.remote).toBe("refs/heads/feature");
    expect(del.local).toBeNull();
    const delInstr = del.instruction();
    expect(delInstr.operation).toBe("push");
    expect(delInstr.kind).toBe("delete");
    // Absent fields on napi objects come through as undefined, not null.
    expect(delInstr.src).toBeUndefined();
    expect(delInstr.dst).toBe("refs/heads/feature");

    // One-sided push: the source doubles as the destination.
    const oneSided = RefSpec.parse("main", RefSpecOperation.Push);
    expect(oneSided.instruction().kind).toBe("matching");
    expect(oneSided.instruction().src).toBe("main");
    expect(oneSided.instruction().dst).toBe("main");
    expect(oneSided.spec).toBe("main:main");

    // Bare colon pushes all matching branches.
    const all = RefSpec.parse(":", RefSpecOperation.Push);
    expect(all.instruction().kind).toBe("all_matching_branches");
    expect(all.instruction().allowNonFastForward).toBe(false);
    expect(all.spec).toBe(":");

    // A forced deletion parses: `+` sets force mode, but the delete instruction
    // itself does not carry the flag (deletions never fast-forward).
    const forcedDel = RefSpec.parse("+:refs/heads/feature", RefSpecOperation.Push);
    expect(forcedDel.mode).toBe("force");
    expect(forcedDel.instruction().kind).toBe("delete");
    expect(forcedDel.instruction().allowNonFastForward).toBe(false);
  });

  test("parses negative fetch specs", () => {
    const spec = RefSpec.parse("^refs/heads/private", RefSpecOperation.Fetch);
    expect(spec.mode).toBe("negative");
    expect(spec.allowNonFastForward()).toBe(false);
    expect(spec.source).toBe("refs/heads/private");
    const instr = spec.instruction();
    expect(instr.operation).toBe("fetch");
    expect(instr.kind).toBe("exclude");
    expect(instr.src).toBe("refs/heads/private");
    expect(instr.dst).toBeUndefined();
  });

  test("negative specs have no prefix", () => {
    expect(RefSpec.parse("^refs/heads/private", RefSpecOperation.Fetch).prefix()).toBeNull();
  });

  test("prefix() and expandPrefixes() expand partial names", () => {
    const partial = RefSpec.parse("main", RefSpecOperation.Fetch);
    // No unambiguous prefix for a partial name.
    expect(partial.prefix()).toBeNull();
    expect(partial.expandPrefixes()).toEqual([
      "main",
      "refs/main",
      "refs/tags/main",
      "refs/heads/main",
      "refs/remotes/main",
      "refs/remotes/main/HEAD",
    ]);

    const exact = RefSpec.parse("refs/heads/main", RefSpecOperation.Fetch);
    expect(exact.prefix()).toBe("refs/heads/main");

    // A glob source needs a matching glob destination on fetch specs.
    const glob = RefSpec.parse("refs/heads/*/suffix:refs/remotes/*/suffix", RefSpecOperation.Fetch);
    expect(glob.prefix()).toBe("refs/heads/");
  });

  test("rejects invalid specs", () => {
    // Empty push specs are invalid (only the fetch side has a HEAD fallback).
    expect(() => RefSpec.parse("", RefSpecOperation.Push)).toThrow();
    // Both sides of a two-sided spec need a pattern.
    expect(() =>
      RefSpec.parse("refs/heads/*:refs/remotes/origin/main", RefSpecOperation.Fetch),
    ).toThrow();
    // More than one '*' per side.
    expect(() =>
      RefSpec.parse("refs/heads/*:refs/remotes/origin/*/*", RefSpecOperation.Fetch),
    ).toThrow();
    // Negative specs cannot have destinations.
    expect(() =>
      RefSpec.parse("^refs/heads/private:refs/tags/private", RefSpecOperation.Fetch),
    ).toThrow();
    // Negative specs must not be object hashes.
    expect(() => RefSpec.parse(`^${sha(0xab)}`, RefSpecOperation.Fetch)).toThrow();
    // Pushing into an empty destination is invalid.
    expect(() => RefSpec.parse("main:", RefSpecOperation.Push)).toThrow();
  });
});

describe("MatchGroup", () => {
  test("parseSpecs() then matchLhs() maps remote refs through fetch patterns", () => {
    const group = MatchGroup.parseSpecs([FETCH_PATTERN], RefSpecOperation.Fetch);
    const mappings = group.matchLhs([
      { fullRefName: "refs/heads/main", target: sha(1) },
      { fullRefName: "refs/tags/v1.0", target: sha(2) },
    ]);
    expect(mappings).toHaveLength(1);
    expect(mappings[0].itemIndex).toBe(0);
    expect(mappings[0].lhs).toBe("refs/heads/main");
    expect(mappings[0].lhsIsObjectId).toBe(false);
    expect(mappings[0].rhs).toBe("refs/remotes/origin/main");
    expect(mappings[0].specIndex).toBe(0);
  });

  test("fromSpecs() builds a group from parsed RefSpec objects", () => {
    const group = MatchGroup.fromSpecs(
      [RefSpec.parse(FETCH_PATTERN, RefSpecOperation.Fetch)],
      RefSpecOperation.Fetch,
    );
    const mappings = group.matchLhs([
      {
        fullRefName: "refs/heads/topic",
        target: sha(3),
      },
    ]);
    expect(mappings).toHaveLength(1);
    expect(mappings[0].rhs).toBe("refs/remotes/origin/topic");
  });

  test("matchLhs() honors exact-name specs and excludes via negation", () => {
    const group = MatchGroup.parseSpecs(
      ["refs/heads/main", "^refs/heads/secret"],
      RefSpecOperation.Fetch,
    );
    const mappings = group.matchLhs([
      { fullRefName: "refs/heads/main", target: sha(1) },
      { fullRefName: "refs/heads/secret", target: sha(2) },
      { fullRefName: "refs/heads/other", target: sha(3) },
    ]);
    // `main` matches the exact spec with no destination; `secret` is removed by the
    // negative spec; `other` matches nothing.
    expect(mappings).toHaveLength(1);
    expect(mappings[0].lhs).toBe("refs/heads/main");
    // No destination on the exact-name spec, so the mapping has no rhs.
    expect(mappings[0].rhs).toBeUndefined();
    expect(mappings[0].specIndex).toBe(0);
  });

  test("matchRhs() reverse-maps tracking branches to their remote counterparts", () => {
    const group = MatchGroup.parseSpecs([FETCH_PATTERN], RefSpecOperation.Fetch);
    const mappings = group.matchRhs([
      {
        fullRefName: "refs/remotes/origin/main",
        target: sha(1),
      },
    ]);
    expect(mappings).toHaveLength(1);
    expect(mappings[0].lhs).toBe("refs/heads/main");
    expect(mappings[0].rhs).toBe("refs/remotes/origin/main");
    expect(mappings[0].itemIndex).toBe(0);
  });

  test("object-id sources are emitted directly without matching items", () => {
    const group = MatchGroup.parseSpecs([`${sha(0xab)}:refs/heads/pinned`], RefSpecOperation.Fetch);
    const mappings = group.matchLhs([]);
    expect(mappings).toHaveLength(1);
    // Object-id mappings are emitted without matching an item.
    expect(mappings[0].itemIndex).toBeUndefined();
    expect(mappings[0].lhs).toBe(sha(0xab));
    expect(mappings[0].lhsIsObjectId).toBe(true);
    expect(mappings[0].rhs).toBe("refs/heads/pinned");
  });

  test("matchLhsValidated() passes clean mappings through with no fixes", () => {
    const group = MatchGroup.parseSpecs([FETCH_PATTERN], RefSpecOperation.Fetch);
    const result = group.matchLhsValidated([
      {
        fullRefName: "refs/heads/main",
        target: sha(1),
      },
    ]);
    expect(result.fixes).toHaveLength(0);
    expect(result.mappings).toHaveLength(1);
    expect(result.mappings[0].rhs).toBe("refs/remotes/origin/main");
  });

  test("matchLhsValidated() rejects conflicting destinations", () => {
    const group = MatchGroup.parseSpecs(
      ["refs/heads/a:refs/tags/x", "refs/heads/b:refs/tags/x"],
      RefSpecOperation.Fetch,
    );
    expect(() =>
      group.matchLhsValidated([
        { fullRefName: "refs/heads/a", target: sha(1) },
        { fullRefName: "refs/heads/b", target: sha(2) },
      ]),
    ).toThrow(/Conflicting destination/);
  });
});
