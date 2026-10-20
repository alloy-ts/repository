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
  ];
  let commitId: string | null = null;
  for (const name of candidates) {
    const ref = repo.findReference(name);
    if (ref && ref.target) {
      commitId = ref.target;
      break;
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
}

main();
