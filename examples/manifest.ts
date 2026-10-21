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

function checkCommitSignatures(repo: Repository) {
  console.log("\n--- Checking Commit Signatures via repo.log() ---");
  try {
    const logs = repo.log(undefined, 10, false);
    for (const entry of logs) {
      const commit = repo.findCommit(entry.id);
      const isSigned = commit.isSigned();
      console.log(`Commit ${entry.id.substring(0, 7)} - "${entry.subject}"`);
      console.log(`  Signed: ${isSigned}`);
      if (isSigned) {
        const verifyOutcome = commit.verifySignature();
        console.log(`  Signature Status: ${verifyOutcome?.status ?? "unknown"}`);
        console.log(`  Valid: ${verifyOutcome?.isValid ?? false}`);
      }
    }
  } catch (err) {
    console.log(`Log / signature check error: ${err}`);
  }
}

function checkDefaultBranchSignature(repo: Repository) {
  console.log("\n--- Checking Default Branch Signature ---");
  const defaultBranch =
    repo.configGet("init.defaultBranch") ??
    repo.configString("init.defaultBranch") ??
    "main";
  console.log(`Configured init.defaultBranch: ${defaultBranch}`);

  let branchRef = null;
  const candidates = [
    `refs/heads/${defaultBranch}`,
    defaultBranch,
    "HEAD",
  ];

  for (const candidate of candidates) {
    try {
      const r = repo.findReference(candidate);
      if (r && r.target) {
        branchRef = r;
        break;
      }
    } catch {
      // continue
    }
  }

  if (branchRef && branchRef.target) {
    console.log(`Found branch reference ${branchRef.name} -> ${branchRef.target}`);
    try {
      const targetCommit = repo.findCommit(branchRef.target);
      const signed = targetCommit.isSigned();
      console.log(`Branch tip commit ${targetCommit.id.substring(0, 7)} is signed: ${signed}`);
      if (signed) {
        const verify = targetCommit.verifySignature();
        console.log(`Branch tip signature verification: valid=${verify?.isValid}, status=${verify?.status}`);
      }
    } catch (e) {
      console.log(`Could not load target commit for branch: ${e}`);
    }
  } else {
    console.log(`Branch refs/heads/${defaultBranch} not found.`);
  }
}

function demonstrateSignatureDto(repo: Repository) {
  console.log("\n--- Demonstrating Repository signatureFromDto() ---");
  const nowInSeconds = Math.floor(Date.now() / 1000);
  const dto = repo.signatureFromDto("Developer", "dev@example.com", nowInSeconds, 0);
  console.log("Constructed Signature DTO via repo.signatureFromDto():", dto);
}

function main() {
  console.log(`Opening repository at: ${repoPath}`);
  const repo = Repository.open(repoPath);

  // 1. Check commit signatures in history
  checkCommitSignatures(repo);

  // 2. Check default branch signature
  checkDefaultBranchSignature(repo);

  // 3. Demonstrate signatureFromDto
  demonstrateSignatureDto(repo);

  // 4. Manifest application
  const manifest = readMetaManifest(repo);
  if (manifest === null) {
    console.log(
      "\nNo `repository.manifest.yml` found under the working tree of `refs/heads/meta/*`.",
    );
    console.log("All refs present:");
    for (const r of repo.references()) {
      console.log(`  ${r.name} -> ${r.target ?? "(symbolic/empty)"}`);
    }
    return;
  }

  console.log("\n--- repository.manifest.yml ---");
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
