import { execSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
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
  base = mkdtempSync(join(tmpdir(), "repo-p3-"));
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
  writeFileSync(join(cloneDir, "p3_fixture.txt"), "p3 fixture payload\n");
  git("add -A", cloneDir);
  git("config commit.gpgsign false", cloneDir);
  git('-c user.email=test@example.com -c user.name=Test commit -m "p3 fixture commit"', cloneDir);
  repo = Repository.open(cloneDir);
});

afterAll(() => {
  rmSync(base, { recursive: true, force: true });
});

describe("P3 Plumbing and ODB Inspection", () => {
  test("emptyTree and emptyBlob return built-in empty objects", () => {
    const tree = repo.emptyTree();
    expect(tree.id).toBeDefined();
    expect(typeof tree.id).toBe("string");

    const blob = repo.emptyBlob();
    expect(blob.id).toBeDefined();
    expect(blob.content().toString()).toBe("");
  });

  test("findHeader and tryFindHeader inspect object metadata", () => {
    const headCommit = repo.headId();
    const header = repo.findHeader(headCommit);
    expect(header.kind).toBe("commit");
    expect(header.size).toBeGreaterThan(0);

    const tryHeader = repo.tryFindHeader(headCommit);
    expect(tryHeader).not.toBeNull();
    expect(tryHeader?.kind).toBe("commit");
  });

  test("writeBlobStream creates a blob via streaming", () => {
    const content = Buffer.from("streaming blob content\n");
    const oid = repo.writeBlobStream(content);
    expect(typeof oid).toBe("string");
    expect(oid.length).toBe(40);
    expect(repo.objectExists(oid)).toBe(true);
  });
});
