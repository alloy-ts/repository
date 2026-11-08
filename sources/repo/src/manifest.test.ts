import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { Manifest, Repository } from "../../repository/src/main.ts";

const SAMPLE_YAML = `
name: test-repo
version: 1.2.3
type: module
owner: test-org
description: A test repository manifest
config:
  - init.defaultBranch: main
  - user.signingKey: ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExampleKey
  - gpg.format: ssh
  - commit.gpgsign: true
remote:
  - https://github.com/test-org/test-repo
  - https://codeberg.org/test-org/test-repo
signers:
  - principal: alice@example.com
    key: ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAliceKey
  - ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBobKey
`;

let tmpDir: string;
let repo: Repository;

beforeAll(() => {
  tmpDir = mkdtempSync(join(tmpdir(), "repo-manifest-test-"));
  repo = Repository.init(tmpDir, false);
});

afterAll(() => {
  rmSync(tmpDir, { recursive: true, force: true });
});

describe("Repository.Manifest (src/repo/manifest.rs)", () => {
  test("Manifest exists under Repository namespace and at top level", () => {
    expect(Repository.Manifest).toBeDefined();
    expect(Manifest).toBe(Repository.Manifest);
    expect(typeof Manifest.parse).toBe("function");
    expect(typeof Manifest.readFromRepo).toBe("function");
  });

  test("Manifest.parse extracts metadata, config, remotes, and signers", () => {
    const manifest = Manifest.parse(SAMPLE_YAML);

    expect(manifest.name).toBe("test-repo");
    expect(manifest.version).toBe("1.2.3");
    expect(manifest.type).toBe("module");
    expect(manifest.owner).toBe("test-org");
    expect(manifest.description).toBe("A test repository manifest");

    // Config entries
    expect(manifest.config).toHaveLength(4);
    expect(manifest.config).toContainEqual({
      key: "init.defaultBranch",
      value: "main",
    });
    expect(manifest.config).toContainEqual({
      key: "commit.gpgsign",
      value: "true",
    });

    // Remotes
    expect(manifest.remotes).toHaveLength(2);
    expect(manifest.remotes[0].name).toBe("github");
    expect(manifest.remotes[0].url).toBe("https://github.com/test-org/test-repo");
    expect(manifest.remotes[1].name).toBe("codeberg");
    expect(manifest.remotes[1].url).toBe("https://codeberg.org/test-org/test-repo");

    // Signers
    expect(manifest.signers).toHaveLength(2);
    expect(manifest.signers[0]).toEqual({
      principal: "alice@example.com",
      key: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAliceKey",
    });
    expect(manifest.signers[1]).toEqual({
      principal: "",
      key: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBobKey",
    });
  });

  test("manifest.applyConfig applies git config to repository", () => {
    const manifest = Manifest.parse(SAMPLE_YAML);
    const result = manifest.applyConfig(repo);

    expect(result.applied).toBeGreaterThan(0);
    const fresh = Repository.open(tmpDir);
    expect(fresh.configString("init.defaultBranch")).toBe("main");
    expect(fresh.configString("gpg.format")).toBe("ssh");

    // Re-applying config on fresh instance
    const secondResult = manifest.applyConfig(fresh);
    expect(secondResult.applied).toBe(0);
    expect(secondResult.unchanged).toBe(manifest.config.length);
  });

  test("manifest.applyRemotes configures remotes on repository", () => {
    const manifest = Manifest.parse(SAMPLE_YAML);
    const result = manifest.applyRemotes(repo);

    expect(result.created).toBe(2);
    expect(result.unchanged).toBe(0);

    const fresh = Repository.open(tmpDir);
    const remotes = fresh.remotes();
    expect(remotes.map((r) => r.name)).toContain("github");
    expect(remotes.map((r) => r.name)).toContain("codeberg");

    // Re-applying unchanged remotes
    const secondResult = manifest.applyRemotes(fresh);
    expect(secondResult.created).toBe(0);
    expect(secondResult.unchanged).toBe(2);
  });

  test("manifest.provisionSigners writes .gitsigners", () => {
    const manifest = Manifest.parse(SAMPLE_YAML);
    const result = manifest.provisionSigners(repo);

    expect(result).not.toBeNull();
    expect(result!.count).toBe(2);
    expect(result!.changed).toBe(true);

    // Second call does not rewrite
    const secondResult = manifest.provisionSigners(repo);
    expect(secondResult!.changed).toBe(false);
  });

  test("Manifest.readFromRepo finds repository.manifest.yml in working directory", () => {
    writeFileSync(join(tmpDir, "repository.manifest.yml"), SAMPLE_YAML, "utf8");

    const found = Manifest.readFromRepo(repo);
    expect(found).not.toBeNull();
    expect(found!.name).toBe("test-repo");
    expect(found!.version).toBe("1.2.3");
  });

  test("toYaml and toJson serialize properly", () => {
    const manifest = Manifest.parse(SAMPLE_YAML);
    const yaml = manifest.toYaml();
    expect(yaml).toContain("name: test-repo");

    const reParsed = Manifest.parse(yaml);
    expect(reParsed.name).toBe(manifest.name);
    expect(reParsed.config).toEqual(manifest.config);

    const json = manifest.toJson();
    const parsedJson = JSON.parse(json);
    expect(parsedJson.name).toBe("test-repo");
    expect(parsedJson.config).toHaveLength(4);
  });
});
