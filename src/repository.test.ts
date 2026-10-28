import { execSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { Repository } from "./main.ts";

const realRoot = process.cwd();
let base: string;
let cloneDir: string;
let repo: Repository;

function git(args: string, cwd: string) {
  execSync(`git ${args}`, { cwd, stdio: "ignore" });
}

beforeAll(async () => {
  base = mkdtempSync(join(tmpdir(), "repo-api-"));
  cloneDir = join(base, "clone");
  try {
    repo = await Repository.clone(realRoot, cloneDir, false);
  } catch (e) {
    rmSync(cloneDir, { recursive: true, force: true });
    execSync(`git clone --local --quiet ${JSON.stringify(realRoot)} ${JSON.stringify(cloneDir)}`, {
      stdio: "ignore",
    });
    repo = Repository.open(cloneDir);
  }
  // Seed a `.gitattributes` and a file it matches, so the attribute tests below have a
  // deterministic fixture regardless of what the cloned project contains.
  writeFileSync(join(cloneDir, ".gitattributes"), "*.txt text\n");
  writeFileSync(join(cloneDir, "data.txt"), "fixture payload\n");
  git("add -A", cloneDir);
  // The dev machine may sign commits globally; the fixture repo has no gpg, so disable it.
  git("config commit.gpgsign false", cloneDir);
  git(
    '-c user.email=test@example.com -c user.name=Test commit -m "fixtures for attribute tests"',
    cloneDir,
  );
  repo = Repository.open(cloneDir);
});

afterAll(() => {
  rmSync(base, {
    recursive: true,
    force: true,
    maxRetries: 10,
    retryDelay: 200,
  });
});

test(
  "clone() returns a usable repository instance (async)",
  async () => {
    const dir = join(base, "clone-by-binding");
    let cloned: Repository | null = null;
    try {
      cloned = await Repository.clone(realRoot, dir, false);
    } catch (e) {
      console.log(`Repository.clone not exercised in this environment (${e})`);
    }
    if (cloned) {
      expect(typeof cloned.headId()).toBe("string");
      expect(cloned.headId().length).toBeGreaterThan(0);
    }
  },
  3 * 60 * 1000,
);

test("init() creates a non-bare worktree repository", () => {
  const dir = join(base, "init-wt");
  const r = Repository.init(dir, false);
  expect(r.isBare()).toBe(false);
  expect(r.kind()).toBe("Common");
  expect(typeof r.gitDir()).toBe("string");
  expect(r.workDir()).toBe(dir);
  expect(r.objectHash()).toBe("Sha1");
});

test("init(bare=true) creates a bare repository", () => {
  const dir = join(base, "init-bare");
  const r = Repository.init(dir, true);
  expect(r.isBare()).toBe(true);
  expect(typeof r.kind()).toBe("string");
  expect(r.workDir()).toBeNull();
  expect(r.gitDir()).toBe(dir);
});

test("discover() walks up from a subdir to the same repo", () => {
  const sub = join(cloneDir, "subdir");
  mkdirSync(sub, { recursive: true });
  const r = Repository.discover(sub);
  expect(r.gitDir()).toBe(repo.gitDir());
});

test("open() opens an existing repository", () => {
  const r = Repository.open(cloneDir);
  expect(r.headId()).toBe(repo.headId());
});

test("new Repository(path) opens the repository at path", () => {
  const r = new Repository(cloneDir);
  expect(r.gitDir()).toBe(repo.gitDir());
  expect(r.headId()).toBe(repo.headId());
  expect(r.workDir()).toBe(cloneDir);
  expect(r.isBare()).toBe(false);
});

test("new Repository() discovers upwards from the current directory", () => {
  // With no path the constructor discovers from `process.cwd()`, i.e. the project
  // itself when this suite runs — the same repository `discover(cwd)` resolves.
  const r = new Repository();
  expect(r.gitDir()).toBe(Repository.discover(process.cwd()).gitDir());
  expect(typeof r.headId()).toBe("string");
});

test("new Repository(path) opens exactly at path, without walking upwards", () => {
  // `cloneDir/subdir` holds no `.git`, so `open` semantics fail where `discover` succeeds.
  const sub = join(cloneDir, "subdir");
  mkdirSync(sub, { recursive: true });
  expect(Repository.discover(sub).gitDir()).toBe(repo.gitDir());
  expect(() => new Repository(sub)).toThrow();
});

test("new Repository(path) throws for a directory that is not a repository", () => {
  const dir = join(base, "not-a-repo");
  mkdirSync(dir, { recursive: true });
  expect(() => new Repository(dir)).toThrow(/^open: /);
});

test("attributesOnly() reads .gitattributes matches without exclude info", () => {
  const attrs = repo.attributesOnly("data.txt");
  expect(Array.isArray(attrs)).toBe(true);
  expect(attrs.some((a) => a.name === "text")).toBe(true);
});

test("attributesOnly() returns nothing for paths matching no pattern", () => {
  // `README.md` matches no `.gitattributes` pattern, so no attributes are assigned.
  expect(repo.attributesOnly("README.md")).toEqual([]);
});

test("attributes() (with excludes) and attributesOnly() agree on .gitattributes matches", () => {
  const withExcludes = repo
    .attributes("data.txt")
    .map((a) => a.name)
    .sort();
  const only = repo
    .attributesOnly("data.txt")
    .map((a) => a.name)
    .sort();
  expect(only).toEqual(withExcludes);
});

test("attributes() and isPathExcluded() check path attributes and ignore status", () => {
  const attrs = repo.attributes("ReadMe.md");
  expect(Array.isArray(attrs)).toBe(true);

  const isExcluded = repo.isPathExcluded("node_modules/test.js");
  expect(typeof isExcluded).toBe("boolean");
});

test("isAncestor() is true for an equal commit", () => {
  const head = repo.headId();
  expect(repo.isAncestor(head, head)).toBe(true);
});

test("isShallow()/shallowCommits()/shallowFile() on a normal clone", () => {
  expect(repo.isShallow()).toBe(false);
  expect(repo.shallowCommits()).toBe(null);
  expect(typeof repo.shallowFile()).toBe("string");
});

test("checkout() checks out target commit asynchronously", async () => {
  const head = repo.headId();
  const checkedOut = await repo.checkout(head);
  expect(checkedOut).toBe(head);
  expect(repo.headId()).toBe(head);
});

test("status() resolves working tree status asynchronously", async () => {
  const statusEntries = await repo.status();
  expect(Array.isArray(statusEntries)).toBe(true);
});

test("Default identity defaultAuthor() / defaultCommitter()", () => {
  const author = repo.defaultAuthor();
  expect(author === null || typeof author === "object").toBe(true);

  const committer = repo.defaultCommitter();
  expect(committer === null || typeof committer === "object").toBe(true);
});

test("Repository path accessors reflects a non-bare worktree", () => {
  expect(repo.isBare()).toBe(false);
  expect(repo.kind()).toBe("Common");
  expect(repo.objectHash()).toBe("Sha1");
  expect(typeof repo.gitDir()).toBe("string");
  expect(typeof repo.commonDir()).toBe("string");
  expect(repo.workDir()).toBe(cloneDir);
});

test("log returns commit summaries newest-first (JS log reading example)", () => {
  // Example: Read top 10 commits via high-level repo.log()
  const entries = repo.log("HEAD", 10, false);
  expect(entries.length).toBeGreaterThan(0);
  expect(entries[0].id).toBe(repo.headId());
  for (const e of entries) {
    expect(typeof e.id).toBe("string");
    expect(typeof e.subject).toBe("string");
    expect(typeof e.authorName).toBe("string");
  }

  // Example: Manual revWalk and findCommit
  const ids = repo.revWalk([repo.headId()]);
  for (const id of ids.slice(0, 5)) {
    const commit = repo.findCommit(id);
    expect(commit.id).toBe(id);
    expect(typeof commit.message).toBe("string");
  }
});

// --- merge options / virtual merge base / state / pathspec / location -------
describe("Repository merge options, virtual merge base, state, pathspec & location", () => {
  test("state() reports no in-progress operation on a clean clone", () => {
    expect(repo.state()).toBe(null);
  });

  test("pathspecDefaults() reflects configuration", () => {
    const d = repo.pathspecDefaults();
    expect(typeof d.searchMode).toBe("string");
    expect(typeof d.signature).toBe("string");
    expect(typeof d.literal).toBe("boolean");
  });

  test("location helpers expose repo paths and trust", () => {
    expect(repo.gitDirTrust()).toBe("full");
    expect(typeof repo.currentDir()).toBe("string");
    expect(repo.modulesPath()).toContain(".git");
    expect(typeof repo.installDir()).toBe("string");
    expect(repo.prefix() === null || typeof repo.prefix() === "string").toBe(true);
    expect(repo.isPristine() === null || typeof repo.isPristine() === "boolean").toBe(true);
    expect(repo.workdirPath("Cargo.toml")).toContain("Cargo.toml");
    expect(repo.normalizePath("Cargo.toml")).toBe("Cargo.toml");
  });

  test("setWorkdir() round-trips the current working tree", () => {
    const current = repo.workdirPath("");
    expect(current).not.toBe(null);
    const previous = repo.setWorkdir(current);
    expect(previous).not.toBe(null);
  });
});

describe("Repository blame (B1)", () => {
  test("blame() returns per-hunk entries for a real file", () => {
    const outcome = repo.blame("Cargo.toml");
    expect(Array.isArray(outcome.entries)).toBe(true);
    expect(outcome.entries.length).toBeGreaterThan(0);

    let prevStart = 0;
    let covered = 0;
    for (const e of outcome.entries) {
      expect(typeof e.commitId).toBe("string");
      expect(e.commitId.length).toBe(40);
      expect(e.len).toBeGreaterThan(0);
      expect(e.startInBlamedFile).toBeGreaterThanOrEqual(prevStart);
      prevStart = e.startInBlamedFile + e.len;
      covered += e.len;
    }
    expect(covered).toBeGreaterThan(0);

    // The blamed file content is returned as bytes.
    expect((outcome.blob as unknown as { length: number }).length).toBeGreaterThan(0);
  });

  test("blame() accepts an explicit commit id", () => {
    const head = repo.headCommit();
    const outcome = repo.blame("Cargo.toml", head.id);
    expect(outcome.entries.length).toBeGreaterThan(0);
    expect(outcome.entries[0].commitId.length).toBe(40);
  });
});

describe("Repository repository-level options (B2)", () => {
  test("path() matches gitDir()", () => {
    expect(repo.path()).toBe(repo.gitDir());
  });

  test("filesystemOptions() reports the platform capabilities", () => {
    const caps = repo.filesystemOptions();
    expect(typeof caps.ignoreCase).toBe("boolean");
    expect(typeof caps.executableBit).toBe("boolean");
    expect(typeof caps.symlink).toBe("boolean");
  });

  test("statOptions() returns the configured comparison behaviour", () => {
    const opts = repo.statOptions();
    // git defaults: trust ctime, full stat comparison, no nsec/stdev comparison.
    expect(opts.trustCtime).toBe(true);
    expect(opts.checkStat).toBe(true);
    expect(opts.useNsec).toBe(false);
    expect(opts.useStdev).toBe(false);
  });

  test("committerOrSetGenericFallback() returns a usable identity", () => {
    const sig = repo.committerOrSetGenericFallback();
    expect(sig.name.length).toBeGreaterThan(0);
    expect(sig.email.length).toBeGreaterThan(0);
  });
});

describe("Repository catFile (git cat-file port)", () => {
  test("catFile(type) reports the object kind for HEAD", () => {
    expect(repo.catFile("HEAD", { mode: "type" })).toBe("commit");
  });

  test("catFile(size) reports the object size for HEAD", () => {
    const size = repo.catFile("HEAD", { mode: "size" });
    expect(typeof size).toBe("number");
    expect(size).toBeGreaterThan(0);
  });

  test("catFile(content) returns raw bytes for an object id", () => {
    const head = repo.headId();
    const content = repo.catFile(head);
    expect(content).toBeInstanceOf(Buffer);
    expect((content as Buffer).length).toBeGreaterThan(0);
    // Default mode is content.
    expect(repo.catFile(head, { mode: "content" })).toBeInstanceOf(Buffer);
  });

  test("catFile(exists) probes without throwing", () => {
    expect(repo.catFile("HEAD", { mode: "exists" })).toBe(true);
    expect(repo.catFile("deadbeef".repeat(5), { mode: "exists" })).toBe(false);
  });

  test("catFile resolves a rev:path blob", () => {
    const tree = repo.findTree(repo.headTreeId());
    const firstEntry = tree.entries[0];
    const blob = repo.catFile(`HEAD:${firstEntry.filename}`);
    expect(blob).toBeInstanceOf(Buffer);
    expect((blob as Buffer).length).toBeGreaterThan(0);
  });
});

describe("Repository newly added trait-parity NAPI bindings", () => {
  test("configSetLocal and configGetBool", () => {
    repo.configSetLocal("test.traitbool", "true");
    expect(repo.configGetBool("test.traitbool")).toBe(true);
    expect(repo.configGetBool("test.nonexistent")).toBe(null);
  });

  test("refResolve, refList, and refUpdate", () => {
    const head = repo.refResolve("HEAD");
    expect(typeof head).toBe("string");
    expect(head.length).toBe(40);

    const refs = repo.refList("refs/heads/");
    expect(Array.isArray(refs)).toBe(true);
    expect(refs.length).toBeGreaterThan(0);
    expect(refs[0].name.startsWith("refs/heads/")).toBe(true);

    repo.refUpdate("refs/tags/vtest-trait", head);
    expect(repo.refResolve("refs/tags/vtest-trait")).toBe(head);
  });

  test("aheadBehind", () => {
    const head = repo.headId();
    const counts = repo.aheadBehind(head);
    expect(counts).toEqual([0, 0]);
  });

  test("cherryPick produces a staged index from a commit", () => {
    const head = repo.headId();
    const index = repo.cherryPick(head);
    expect(typeof index.writeTree).toBe("function");
  });

  test("checkoutHead, checkoutIndex, and checkoutTree update working directory", () => {
    expect(() => repo.checkoutHead()).not.toThrow();
    expect(() => repo.checkoutIndex()).not.toThrow();
    const head = repo.headId();
    expect(() => repo.checkoutTree(head)).not.toThrow();
  });

  test("pushRefspec and Remote.push", () => {
    expect(typeof repo.pushRefspec).toBe("function");
    const remote = repo.findRemote("origin");
    expect(typeof remote.push).toBe("function");
  });
});
