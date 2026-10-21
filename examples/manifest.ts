import { load } from "js-yaml";
import { Repository } from "../src/main.ts";

const repoPath = process.argv[2] ?? process.cwd();

/** Read `repository.manifest.yml`, preferring `repository/` then the tree root. */
function readMetaManifest(repo: Repository): string | null {
  const candidates = [
    "refs/heads/meta/repository",
    "refs/heads/meta/repo",
    "refs/heads/meta",
  ];
  let commitId: string | null = null;
  for (const name of candidates) {
    const ref = repo.tryFindReference(name);
    if (ref && ref.target) {
      commitId = ref.target;
      break;
    }
  }
  if (!commitId) return null;

  let tree = repo.findTree(repo.findCommit(commitId).treeId);

  const repoDir = tree.entry("repository");
  if (repoDir && repoDir.mode === "040000") {
    tree = repo.findTree(repoDir.id);
  }

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

/** Extract `config:` section from manifest YAML. */
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
      console.log(`  ${r.name} -> ${r.target || "(symbolic/empty)"}`);
    }
  } else {
    console.log("--- repository.manifest.yml ---");
    console.log(manifest);

    const configs = parseManifestConfigs(manifest);
    console.log(`\nApplying ${configs.length} config(s) via repo.configSet():`);
    for (const [key, value] of configs) {
      if (repo.configGet(key) === value) {
        console.log(`  (unchanged) ${key} = ${value}`);
        continue;
      }
      repo.configSet(key, value);
      console.log(`  set ${key} = ${value}`);
    }
  }

  console.log("\n--- Checking Recent Commit Signatures via repo.log() ---");
  const logs = repo.log("HEAD", 5, false);
  for (const entry of logs) {
    const isSigned = repo.isCommitSigned(entry.id);
    console.log(`  Commit ${entry.id.slice(0, 7)} (${entry.subject}): signed = ${isSigned}`);
  }

  console.log("\n--- Checking Default Branch & Signature ---");
  const defaultBranch = repo.configGet("init.defaultBranch") ?? "main";
  console.log(`Configured default branch (init.defaultBranch): ${defaultBranch}`);
  const branchRef = repo.tryFindReference(`refs/heads/${defaultBranch}`);
  if (branchRef && branchRef.target) {
    const branchSigned = repo.isCommitSigned(branchRef.target);
    console.log(`  Branch '${branchRef.name}' target (${branchRef.target.slice(0, 7)}): signed = ${branchSigned}`);
    try {
      const verify = repo.verifyCommitSignature(branchRef.target);
      if (verify) {
        console.log(`    Verification status: ${verify.status}, isValid: ${verify.isValid}`);
      }
    } catch (err) {
      console.log(`    Verification note: ${err}`);
    }
  } else {
    console.log(`  Branch 'refs/heads/${defaultBranch}' not found.`);
  }

  console.log("\n--- Validating Signature DTO via repo.signatureFromDto() ---");
  const validatedSig = repo.signatureFromDto({
    name: "Repository Manifest Bot",
    email: "bot@example.com",
    time: Math.floor(Date.now() / 1000),
    timezoneOffset: 0,
  });
  console.log(`  Validated signature: ${validatedSig.name} <${validatedSig.email}> (${validatedSig.time})`);
}

main();
