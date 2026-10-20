import { describe, expect, test } from "vite-plus/test";

import { RefSpec, RefSpecOperation } from "./main.ts";

describe("RefSpec parsing and sides", () => {
  test("parses a force fetch refspec and exposes its sides", () => {
    const ps = RefSpec.parse("+refs/heads/*:refs/remotes/origin/*", RefSpecOperation.Fetch);
    expect(ps.operation).toBe(RefSpecOperation.Fetch);
    expect(ps.mode).toBe("force");
    expect(ps.allowNonFastForward()).toBe(true);
    expect(ps.source).toBe("refs/heads/*");
    expect(ps.destination).toBe("refs/remotes/origin/*");
    expect(ps.local).toBe("refs/remotes/origin/*");
    expect(ps.remote).toBe("refs/heads/*");
    expect(ps.spec).toContain("refs/heads/*");
    expect(ps.instruction().kind).toBe("and_update");
  });

  test("parses a negative fetch refspec", () => {
    const ps = RefSpec.parse("^refs/heads/secret", RefSpecOperation.Fetch);
    expect(ps.mode).toBe("negative");
    expect(ps.source).toBe("refs/heads/secret");
    expect(ps.destination).toBeNull();
  });
});

describe("RefSpec name matching and mapping", () => {
  const fetch = RefSpec.parse("+refs/heads/*:refs/remotes/origin/*", RefSpecOperation.Fetch);

  test("sourceMatches / destinationMatches probe each side", () => {
    expect(fetch.sourceMatches("refs/heads/main")).toBe(true);
    expect(fetch.sourceMatches("refs/tags/v1")).toBe(false);
    expect(fetch.destinationMatches("refs/remotes/origin/main")).toBe(true);
    expect(fetch.destinationMatches("refs/heads/main")).toBe(false);
  });

  test("transform / rtransform map between the two sides", () => {
    expect(fetch.transform("refs/heads/main")).toBe("refs/remotes/origin/main");
    expect(fetch.rtransform("refs/remotes/origin/main")).toBe("refs/heads/main");
    expect(fetch.transform("refs/tags/v1")).toBeNull();
    expect(fetch.rtransform("refs/heads/main")).toBeNull();
  });

  test("a negative refspec does not produce observable matches", () => {
    const negative = RefSpec.parse("^refs/heads/secret", RefSpecOperation.Fetch);
    // git2's src_matches is purely syntactic and would be true here; gix only operates on
    // mappings, and a negation never yields one, so this mirrors the mapping model.
    expect(negative.sourceMatches("refs/heads/secret")).toBe(false);
    expect(negative.transform("refs/heads/secret")).toBeNull();
  });

  test("a one-sided push refspec matches its source but has no destination", () => {
    const push = RefSpec.parse("refs/heads/main", RefSpecOperation.Push);
    expect(push.operation).toBe(RefSpecOperation.Push);
    expect(push.source).toBe("refs/heads/main");
    expect(push.destination).toBeNull();
    expect(push.sourceMatches("refs/heads/main")).toBe(true);
    expect(push.destinationMatches("refs/heads/main")).toBe(false);
    expect(push.transform("refs/heads/main")).toBeNull();
  });

  test("a full push refspec maps both directions", () => {
    const push = RefSpec.parse("refs/heads/main:refs/heads/pub", RefSpecOperation.Push);
    expect(push.sourceMatches("refs/heads/main")).toBe(true);
    expect(push.destinationMatches("refs/heads/pub")).toBe(true);
    expect(push.transform("refs/heads/main")).toBe("refs/heads/pub");
    expect(push.rtransform("refs/heads/pub")).toBe("refs/heads/main");
  });
});
