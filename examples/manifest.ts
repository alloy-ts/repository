import { load } from "js-yaml";
import { Repository } from "../src/main.ts";

const repoPath = process.argv[2] ?? process.cwd();

/** Read `repository.manifest.yml`, preferring `repository/` then the tree root. */
function readMetaManifest(repo: Repository): string | null {
  // Resolve the metadata commit. `findReference` returns null instead of throwing when
  // the ref is absent.
  const candidates = [
    "refs/heads/meta/repository",
    "refs/heads/meta/repo",
    "refs/heads/meta",
    "refs/remotes/origin/meta/repository",
    "refs/remotes/origin/meta/repo",
    "refs/remotes/origin/meta",
  ];
  let commitId: string | null = null;
  for (const name of candidates) {
    try {
      const ref = repo.findReference(name);
      if (ref && ref.target) {
        commitId = ref.target;
        break;
      }
    } catch {
      // ref not found
    }
  }
  if (!commitId) return null;

  // commit -> its root tree
  let tree = repo.findTree(repo.findCommit(commitId).treeId);

  // Drill into the `repository/` directory when present.
  const repoDir = tree.entry("repository");
  if (repoDir && repoDir.mode === "040000") {
    tree = repo.findTree(repoDir.id);
  }

  // Candidate filenames, most-specific first.
  const fileCandidates = [
    "repository.manifest.yml",
    "repository.manifest.yaml",
    ".repository.manifest.yml",
    ".repository.manifest.yaml",
  ];
  for (const name of fileCandidates) {
    const entry = tree.entry(name);
    if (entry) {
      return repo.findBlob(entry.id).content().toString("utf-8");
    }
  }
  return null;
}

/**
 * Extract the `config:` section from the manifest as `[key, value]` pairs.
 * The manifest expresses config as a YAML list of single-key maps, e.g.
 *   config:
 *     - user.signingKey: ssh-ed25519 AAAA...
 *     - gpg.format: ssh
 */
function parseManifestConfigs(text: string): Array<[string, string]> {
  const doc = load(text) as { config?: Array<Record<string, unknown>> };
  const pairs: Array<[string, string]> = [];
  for (const item of doc.config ?? []) {
    for (const [key, value] of Object.entries(item)) {
      pairs.push([key, String(value)]);
    }
  }
  return pairs;
}

function main() {
  console.log(`Opening repository at: ${repoPath}`);
  const repo = Repository.open(repoPath);

  const manifest = readMetaManifest(repo);
  if (manifest === null) {
    console.log(
      "No `repository.manifest.yml` found under the working tree of `refs/heads/meta/*`.",
    );
    console.log("All refs present:");
    for (const r of repo.references()) {
      console.log(`  ${r.name} -> ${r.target ?? "(symbolic/empty)"}`);
    }
    return;
  }

  console.log("--- repository.manifest.yml ---");
  console.log(manifest);

  const configs = parseManifestConfigs(manifest);
  console.log(`\nApplying ${configs.length} config(s) via repo.configSet():`);
  for (const [key, value] of configs) {
    // Skip when the local config already holds the same value (avoids redundant writes).
    if (repo.configString(key) === value) {
      console.log(`  (unchanged) ${key} = ${value}`);
      continue;
    }
    repo.configSet(key, value);
    console.log(`  set ${key} = ${value}`);
  }

  // Check commit signatures via repo.log()
  console.log("\nChecking recent commits signature status via repo.log():");
  const recentCommits = repo.log(undefined, 5);
  for (const entry of recentCommits) {
    console.log(
      `  commit ${entry.id.slice(0, 7)}: "${entry.subject}" [signed: ${entry.isSigned}]`,
    );
  }

  // Verify branch & signature on default branch
  const defaultBranch = repo.configString("init.defaultBranch") ?? "main";
  const branchRefName = `refs/heads/${defaultBranch}`;
  try {
    const branch = repo.findReference(branchRefName);
    if (branch && branch.target) {
      console.log(`\nFound default branch ref: ${branchRefName} -> ${branch.target}`);
      const commit = repo.findCommit(branch.target);
      console.log(
        `  commit ${commit.id.slice(0, 7)} signature status: isSigned = ${commit.isSigned}`,
      );
    }
  } catch {
    console.log(`\nDefault branch ref not found: ${branchRefName}`);
  }
}

main();
