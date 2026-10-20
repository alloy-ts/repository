import { Repository } from "../src/main.ts";

const repoPath = process.argv[2] ?? process.cwd();

const short = (id: string) => id.slice(0, 8);

function main() {
  console.log(`Opening repository at: ${repoPath}`);
  const repo = Repository.open(repoPath);
  const head = repo.headId();

  // ---------------------------------------------------------------------------
  // Resolving specs: everything git's rev-parse accepts — object ids, HEAD,
  // branch/tag names, and `~`/`^`/`^{}` navigation.
  // ---------------------------------------------------------------------------
  console.log("\n=== Rev-parse ===");
  console.log(`revParseSingle("HEAD")      -> ${short(repo.revParseSingle("HEAD"))}`);
  console.log(
    `revParseSingle("HEAD~1")    -> ${
      repo.headCommit().parentIds[0]
        ? short(repo.revParseSingle("HEAD~1"))
        : "(root commit, no parent)"
    }`,
  );
  console.log(`revParseHex("HEAD^{tree}")  -> ${short(repo.revParseHex("HEAD^{tree}"))}`);

  // revParse reports the spec's structure, not just its result.
  console.log(`revParse("HEAD")            -> ${JSON.stringify(repo.revParse("HEAD"))}`);
  const parent = repo.headCommit().parentIds[0];
  if (parent) {
    console.log(`revParse("HEAD~1..HEAD")    -> ${JSON.stringify(repo.revParse("HEAD~1..HEAD"))}`);
  }
  console.log(`revParse("^HEAD")           -> kind=${repo.revParse("^HEAD").kind}`);

  // revParseExt also tells you which reference provided the resolution.
  const ext = repo.revParseExt("HEAD");
  console.log(`revParseExt("HEAD")         -> id=${short(ext.id)} ref=${ext.referenceName}`);
  const byOid = repo.revParseExt(head);
  console.log(`revParseExt(<head id>)      -> ref=${byOid.referenceName ?? "null"}`);

  // ---------------------------------------------------------------------------
  // Rev-walk: one-shot traversal of the commit graph. Tips may be any spec;
  // options add sorting, reversal, first-parent simplification, exclusions,
  // reference globs, and ranges.
  // ---------------------------------------------------------------------------
  console.log("\n=== Rev-walk ===");
  const all = repo.revWalk(["HEAD"]);
  console.log(`revWalk(["HEAD"])           -> ${all.length} commit(s), tip ${short(all[0])}`);

  if (parent) {
    const viaExclude = repo.revWalk(["HEAD"], { exclude: [parent] });
    const viaRange = repo.revWalk([], { range: `${parent}..HEAD` });
    console.log(`exclude ["${short(parent)}"]  -> ${viaExclude.length} commit(s) after the parent`);
    console.log(
      `range "${short(parent)}..HEAD" -> same list: ${
        JSON.stringify(viaRange) === JSON.stringify(viaExclude)
      }`,
    );
    const symmetric = repo.revWalk([], { range: `${parent}...HEAD` });
    console.log(
      `symmetric "...":            -> ${symmetric.length} commit(s) reachable from either side, not both`,
    );
  }

  // A glob pushes every matching reference as a tip; "refs/" and "/*" are implied.
  const tagTips = repo.revWalk([], { glob: "refs/tags/*" });
  console.log(`glob "tags/*"               -> ${tagTips.length} tag tip(s)`);

  console.log(
    `sort + reverse              -> newest-first: ${
      repo.revWalk(["HEAD"], { sort: "time-newest", reverse: true })[0] === all[all.length - 1]
    }`,
  );
  const firstParent = repo.revWalk(["HEAD"], { firstParent: true });
  console.log(`firstParent: true           -> ${firstParent.length} of ${all.length} commit(s)`);

  try {
    repo.revWalk(["HEAD"], { sort: "topo" });
  } catch (e) {
    console.log(`invalid sort rejected:      -> ${(e as Error).message.split("\n")[0]}`);
  }

  // ---------------------------------------------------------------------------
  // Describe: `git describe` — the closest tag or branch containing a commit.
  // ---------------------------------------------------------------------------
  console.log("\n=== Describe ===");
  const describeTarget = parent ?? head;
  const described = repo.describe(describeTarget, {
    names: "allTags",
    idAsFallback: true,
  });
  if (described) {
    console.log(`describe(${short(describeTarget)}) ->`);
    console.log(`  name: ${described.name ?? "(none)"} depth: ${described.depth}`);
    console.log(`  format: ${described.format} (exact: ${described.exactMatch})`);
    console.log(`  commitsSeen: ${described.commitsSeen}`);
  }
  // Without idAsFallback a commit with no candidate name yields null.
  console.log(
    `describe(maxCandidates: 0)  -> ${repo.describe(describeTarget, { maxCandidates: 0 })}`,
  );
}

main();
