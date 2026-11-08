import { Manifest, Repository } from "../sources/repository/src/main.ts";

const repoPath = process.argv[2] ?? process.cwd();

function main() {
  console.log(`Opening repository at: ${repoPath}`);
  const repo = Repository.open(repoPath);

  // Read manifest using Repository.Manifest.readFromRepo (prefers working tree, falls back to meta/* branches)
  const manifest = Manifest.readFromRepo(repo);
  if (!manifest) {
    console.log(
      "No `repository.manifest.yml` found under the working tree or `refs/heads/meta/*`.",
    );
    console.log("All refs present:");
    for (const r of repo.references()) {
      console.log(`  ${r.name} -> ${r.target ?? "(symbolic/empty)"}`);
    }
    return;
  }

  console.log("--- repository.manifest.yml ---");
  console.log(manifest.raw);

  // Apply configs
  const configResult = manifest.applyConfig(repo);
  console.log(
    `\nApplied config(s): ${configResult.applied} set, ${configResult.unchanged} unchanged.`,
  );

  // Apply remotes
  const remoteResult = manifest.applyRemotes(repo);
  console.log(
    `\nConfigured remote(s): ${remoteResult.created} created, ${remoteResult.updated} updated, ${remoteResult.unchanged} unchanged.`,
  );

  // Provision .gitsigners
  const signersResult = manifest.provisionSigners(repo);
  if (signersResult) {
    console.log(
      `\nProvisioned ${signersResult.count} signer(s) to ${signersResult.path} (changed: ${signersResult.changed})`,
    );
  } else {
    console.log("\nNo `signers:` section; skipping .gitsigners provisioning.");
  }

  // Verify the default branch tip signature if signed
  const defaultBranch = repo.configString("init.defaultBranch") ?? "main";
  const branch = repo.findReference(`refs/heads/${defaultBranch}`);
  if (branch?.target) {
    try {
      const outcome = repo.findCommit(branch.target).verify();
      if (outcome) {
        console.log(
          `\nDefault branch '${defaultBranch}' tip ${branch.target.slice(0, 8)} is signed: ` +
            `status=${outcome.status} trustLevel=${outcome.trustLevel} signer=${
              outcome.signer ?? "?"
            }`,
        );
      } else {
        console.log(`\nDefault branch '${defaultBranch}' tip is not signed.`);
      }
    } catch (e) {
      console.log(`\nSignature verify note: ${(e as Error).message}`);
    }
  } else {
    console.log(`\nNo ref refs/heads/${defaultBranch} found.`);
  }

  // Recent commit signature status
  console.log("\nRecent commit status:");
  for (const c of repo.log(undefined, 5)) {
    console.log(`  ${(c.sha ?? c.shortSha).slice(0, 8)}  ${c.subject}`);
  }
}

main();
