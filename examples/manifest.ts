import { load } from "js-yaml";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
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

function nameFromUrl(url: string): string {
  let host: string | undefined;
  try {
    host = new URL(url).hostname;
  } catch {
    // Fall back for scp-like `git@host:path` or `git://host/...` URLs.
    const m = url.match(/(?:^|@|:\/\/)([^:/]+)(?::|\/|$)/);
    host = m?.[1];
  }
  if (!host) return "origin";
  const labels = host.split(".");
  const sld = labels.length >= 2 ? labels[labels.length - 2] : labels[0];
  return sld ?? "origin";
}

function parseManifestRemotes(
  text: string,
): Array<{ name: string; url: string; fetch?: string }> {
  const doc = load(text) as { remote?: unknown; remotes?: unknown };
  const out: Array<{ name: string; url: string; fetch?: string }> = [];

  const add = (value: unknown) => {
    if (typeof value === "string") {
      out.push({ name: nameFromUrl(value), url: value });
    } else if (value && typeof value === "object") {
      const obj = value as Record<string, unknown>;
      if (typeof obj["url"] !== "string") return;
      out.push({
        name: typeof obj["name"] === "string" ? obj["name"] : nameFromUrl(obj["url"]),
        url: obj["url"],
        fetch: typeof obj["fetch"] === "string" ? obj["fetch"] : undefined,
      });
    }
  };

  if (doc.remote !== undefined) {
    if (Array.isArray(doc.remote)) doc.remote.forEach(add);
    else add(doc.remote);
  }
  if (Array.isArray(doc.remotes)) doc.remotes.forEach(add);

  return out;
}

function parseManifestSigners(
  text: string,
): Array<{ principal: string; key: string }> {
  const doc = load(text) as { signers?: unknown };
  const out: Array<{ principal: string; key: string }> = [];
  const add = (value: unknown) => {
    if (typeof value === "string") {
      out.push({ principal: "", key: value });
    } else if (value && typeof value === "object") {
      const obj = value as Record<string, unknown>;
      if (typeof obj["key"] !== "string") return;
      const principal =
        typeof obj["principal"] === "string"
          ? obj["principal"]
          : typeof obj["email"] === "string"
            ? obj["email"]
            : "";
      out.push({ principal, key: obj["key"] });
    }
  };
  if (Array.isArray(doc.signers)) doc.signers.forEach(add);
  return out;
}

/**
 * Provision `.gitsigners` (the file named by `gpg.ssh.allowedSignersFile`) from the
 * manifest's `signers:` section so SSH signature verification can reach
 * `trustLevel=Fully`. Written to the repository working dir (fallback: git dir) to
 * match where gix resolves the relative path.
 */
function provisionAllowedSigners(repo: Repository, manifest: string) {
  const signers = parseManifestSigners(manifest);
  if (signers.length === 0) {
    console.log("\nNo `signers:` section; skipping .gitsigners provisioning.");
    return;
  }
  const base = repo.workDir() ?? repo.gitDir();
  const filePath = join(base, ".gitsigners");
  const content =
    signers
      .map((s) =>
        s.principal ? `${s.principal} namespaces="git" ${s.key}` : s.key,
      )
      .join("\n") + "\n";

  try {
    if (existsSync(filePath) && readFileSync(filePath, "utf8") === content) {
      console.log(
        `\n(unchanged) ${signers.length} signer(s) in .gitsigners at ${filePath}`,
      );
      return;
    }
    writeFileSync(filePath, content, "utf8");
    console.log(
      `\nwrote ${signers.length} signer(s) to .gitsigners at ${filePath}`,
    );
  } catch (e) {
    console.log(`\nfailed to write .gitsigners: ${(e as Error).message}`);
  }
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

  // --- Configure remotes declared in the manifest via repo.createRemote() ---
  const remotes = parseManifestRemotes(manifest);
  console.log(`\nConfiguring ${remotes.length} remote(s) via repo.createRemote():`);
  const existingRemotes = repo.remotes();
  for (const { name, url, fetch } of remotes) {
    const existing = existingRemotes.find((r) => r.name === name);
    if (existing && existing.url === url) {
      console.log(`  (unchanged) remote '${name}' -> ${url}`);
    } else {
      try {
        repo.createRemote(name, url);
        if (fetch) repo.configSet(`remote.${name}.fetch`, fetch);
        console.log(`  created remote '${name}' -> ${url}`);
      } catch (e) {
        console.log(`  failed to create remote '${name}': ${(e as Error).message}`);
      }
    }
  }

  // --- Provision .gitsigners so SSH signature verification can resolve trust ---
  provisionAllowedSigners(repo, manifest);

  // --- Verify the default branch tip signature via repo.findCommit(...).verify() ---
  const defaultBranch = repo.configString("init.defaultBranch") ?? "main";
  const branch = repo.findReference(`refs/heads/${defaultBranch}`);
  if (branch?.target) {
    const outcome = repo.findCommit(branch.target).verify();
    if (outcome) {
      console.log(
        `\nDefault branch '${defaultBranch}' tip ${branch.target.slice(0, 8)} is signed: ` +
          `status=${outcome.status} trustLevel=${outcome.trustLevel} signer=${outcome.signer ?? "?"}`,
      );
    } else {
      console.log(`\nDefault branch '${defaultBranch}' tip is not signed.`);
    }
  } else {
    console.log(`\nNo ref refs/heads/${defaultBranch} found.`);
  }

  // --- Recent commit signature status via repo.log() + verify() ---
  console.log("\nRecent commit signature status:");
  for (const c of repo.log(undefined, 10)) {
    const outcome = repo.findCommit(c.id).verify();
    const signed = outcome ? `${outcome.status}/${outcome.trustLevel}` : "unsigned";
    console.log(`  ${c.id.slice(0, 8)}  ${signed}  ${c.subject}`);
  }
}

main();
