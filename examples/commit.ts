import { execSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  type Commit,
  formatTrailer,
  formatTrailers,
  Repository,
  type Signature,
} from "../src/main.ts";

// Demonstrates the Commit API exposed in src/commit.rs: reading commit fields,
// walking parents and history, inspecting the tree and headers, writing new
// commits (object-only and reference-moving), amending, and signing/verifying.
// It builds a throwaway repository so it is safe to create, amend, and sign
// commits as it goes.

const base = mkdtempSync(join(tmpdir(), "commit-example-"));
const repoPath = join(base, "scratch-repo");
// `git init` creates the directory itself; the next commands need it to exist.
execSync(`git init --quiet --initial-branch=main ${JSON.stringify(repoPath)}`, {
  stdio: "ignore",
});
const git = (args: string) => execSync(`git ${args}`, { cwd: repoPath, stdio: "ignore" });
git("config user.email commit@example.com");
git("config user.name Commit Example");
// The throwaway repo inherits the ambient git config; signing is irrelevant here
// and would fail in environments without a usable key.
git("config commit.gpgsign false");
git("config tag.gpgsign false");

// Commit 1: a subject plus a body with a trailer, so `body` / `trailers` have
// something to parse.
writeFileSync(join(repoPath, "README.md"), "# commit example\n");
git("add README.md");
// Each `-m` becomes a separate paragraph; the trailer ends up in the body so
// `body` / `trailers` / `coAuthors` have something to parse.
git(
  "commit --quiet -m 'Initial commit' -m 'Project bootstrap.' -m 'Signed-off-by: Commit Example <commit@example.com>'",
);

// Commit 2: a second commit so HEAD has a parent and we can walk history.
writeFileSync(join(repoPath, "src.txt"), "src\n");
git("add src.txt");
git("commit --quiet -m 'Add source'");

const repo = Repository.open(repoPath);
const short = (id: string) => id.slice(0, 8);

// ---------------------------------------------------------------------------
// 1. Basic fields: id, message, summary, subject, body, time, author, committer
// ---------------------------------------------------------------------------
console.log("=== 1. Basic fields ===");
const head: Commit = repo.headCommit();
console.log(`id                        = ${head.id}`);
console.log(`shortSha (>= 7 chars)    = ${head.shortSha}`);
console.log(`message                  = ${JSON.stringify(head.message)}`);
console.log(`messageRaw               = ${JSON.stringify(head.messageRaw)}`);
console.log(`summary                  = ${head.summary ?? "(none)"}`);
console.log(`subject                  = ${head.subject}`);
console.log(`body                     = ${head.body ?? "(none)"}`);
console.log(`encoding                 = ${head.encoding ?? "utf-8 (default)"}`);

// `time` is a { seconds, offset } record. NOTE: `offset` is in **seconds**
// here (git2/libgit2 reports minutes), and `authorDate` / `committerDate` are
// pre-rendered RFC 3339 strings.
const time = head.time;
console.log(`time.seconds             = ${time.seconds}`);
console.log(`time.offset (seconds)    = ${time.offset}`);
console.log(`date (ms)                = ${time.seconds * 1000}`);
console.log(`authorDate               = ${head.authorDate}`);
console.log(`committerDate            = ${head.committerDate}`);

const author: Signature = head.author();
const committer: Signature = head.committer();
console.log(
  `author                   = ${author.name} <${author.email}> ${author.time} ${
    author.timezoneOffset >= 0 ? "+" : ""
  }${author.timezoneOffset}s`,
);
console.log(
  `committer                = ${committer.name} <${committer.email}> ${
    committer.timezoneOffset >= 0 ? "+" : ""
  }${committer.timezoneOffset}s`,
);

// ---------------------------------------------------------------------------
// 2. Parents and history
// ---------------------------------------------------------------------------
console.log("\n=== 2. Parents and history ===");
console.log(`parentCount              = ${head.parentCount()}`);
console.log(`parentIds                = [${head.parentIds.map(short).join(", ")}]`);

const parent = head.parent(0);
console.log(`parent(0).id             = ${parent.id}`);
console.log(
  `parent(0) subject        = ${parent.subject} (== "${repo.findCommit(parent.id).subject}")`,
);

// The tree of a parent commit resolves through the repository.
const parentTree = parent.tree();
console.log(`parent(0).tree().id      = ${parentTree.id}`);
console.log(`parent(0) tree entries   = ${parentTree.entries.length}`);

// All parents at once.
const parents = head.parents();
console.log(`parents().length         = ${parents.length}`);

// History is a synchronous list (newest first) of CommitInfo snapshots. For an
// event-stream style walk, see the `log` example; here we just summarise.
const history = repo.log();
console.log(`repo.log()               = ${history.length} commit(s)`);
for (const entry of history.slice(0, 5)) {
  console.log(`  ${short(entry.sha)} ${entry.subject}`);
}

// ---------------------------------------------------------------------------
// 3. Tree and tree diff
// ---------------------------------------------------------------------------
console.log("\n=== 3. Tree and tree diff ===");
const tree = head.tree();
console.log(`commit.tree().id         = ${tree.id}`);
console.log(`commit.treeId            = ${head.treeId}`);

// tree.walk() is a depth-first flattening of every entry with its full path.
const walked = tree.walk();
console.log(`tree.walk()              = ${walked.length} entr(ies) total`);
for (const e of walked.slice(0, 5)) {
  console.log(`  ${e.mode} ${e.id.slice(0, 8)} ${e.path}`);
}

// The diff of a commit is the diff between its tree and its parent's tree.
if (head.parentCount() > 0) {
  const changes = repo.diffTreeToTree(parentTree.id, tree.id);
  console.log(`diff vs parent          = ${changes.length} change(s):`);
  for (const c of changes) {
    console.log(`  ${c.kind} ${c.path} (${c.oldId?.slice(0, 8)} -> ${c.newId?.slice(0, 8)})`);
  }
}

// ---------------------------------------------------------------------------
// 4. Header fields
// ---------------------------------------------------------------------------
console.log("\n=== 4. Header fields ===");
console.log(`headerField("parent")    = ${head.headerField("parent") ?? "(none)"}`);
console.log(`headerField("author")    = ${head.headerField("author")}`);
console.log(`headerField("committer") = ${head.headerField("committer")}`);
console.log(`extraHeaders             = ${head.extraHeaders().length} custom header(s)`);
console.log(`rawHeader (first line)   = ${head.rawHeader.split("\n")[0]}`);

// ---------------------------------------------------------------------------
// 5. nth ancestor (follows first-parent history)
// ---------------------------------------------------------------------------
console.log("\n=== 5. Ancestors ===");
console.log(`nthAncestor(0).id        = ${head.nthAncestor(0).id} (this commit)`);
console.log(`nthAncestor(1).id        = ${head.nthAncestor(1).id} (== parent)`);

// ---------------------------------------------------------------------------
// 6. Writing commits (object-only and reference-moving)
// ---------------------------------------------------------------------------
console.log("\n=== 6. Writing commits ===");

// newCommit() / writeCommit() write a loose object and return its id WITHOUT
// moving any reference — useful for creating commits programmatically.
const newBlob = repo.writeBlob(Buffer.from("created by examples/commit.ts\n"));
const newTree = repo.writeTree([
  {
    path: "generated.txt",
    mode: "100644",
    id: newBlob,
  },
]);
const detachedId = repo.newCommit(
  "Detached commit written to the ODB\n\nNo reference was moved.\n",
  newTree,
  [head.id],
);
console.log(`newCommit()              = ${short(detachedId)}`);
console.log(`  HEAD still points at     = ${short(repo.headCommit().id)} (unchanged)`);

// writeCommit() accepts explicit author/committer identities and extra headers.
const signedOff = repo.writeCommit({
  tree: newTree,
  parents: [head.id],
  author,
  committer,
  message: "Loose commit with an extra header\n",
  extraHeaders: [
    {
      name: "Reviewed-by",
      value: "Reviewer <reviewer@example.com>",
    },
  ],
});
const loose = repo.findCommit(signedOff);
console.log(`writeCommit()            = ${short(signedOff)}`);
console.log(`  extra header present    = ${loose.headerField("Reviewed-by")}`);

// commit() / commitAs() move a reference (here HEAD, which dereferences to the
// current branch) and return the new commit id.
const movedId = repo.commit("HEAD", "Commit that moves HEAD", newTree, [head.id]);
console.log(`commit("HEAD", ...)       = ${short(movedId)}`);
console.log(`  new HEAD                = ${short(repo.headCommit().id)}`);

const explicit: Signature = {
  name: "Explicit Author",
  email: "explicit@e.com",
  time: 1_700_000_000,
  timezoneOffset: 0,
};
const movedById = repo.commitAs(
  explicit,
  explicit,
  "HEAD",
  "Commit with explicit author/committer",
  newTree,
  [movedId],
);
console.log(`commitAs(...)            = ${short(movedById)}`);
const explicitCommit = repo.findCommit(movedById);
console.log(
  `  author of new commit     = ${explicitCommit.author().name} <${explicitCommit.author().email}>`,
);

// commitAll() is the high-level `git commit -a -m`: it stages every worktree
// change, derives the tree from the index, and commits on the current branch.
writeFileSync(join(repoPath, "more.txt"), "more\n");
const allId = repo.commitAll({ message: "Commit all the worktree changes" });
console.log(`commitAll({message})     = ${short(allId)}`);
console.log(`  new HEAD subject        = ${repo.headCommit().subject}`);

// ---------------------------------------------------------------------------
// 7. Amending
// ---------------------------------------------------------------------------
console.log("\n=== 7. Amending ===");

// amend() with `updateRef: "HEAD"` rewrites the commit AND moves the branch
// HEAD points at, exactly like `git commit --amend`.
const amendId = repo.headCommit().amend({
  message: "Amended message",
  updateRef: "HEAD",
});
console.log(`amend(message, "HEAD")   = ${short(amendId)}`);
console.log(`  new HEAD                = ${short(repo.headCommit().id)}`);
console.log(`  message now             = ${JSON.stringify(repo.headCommit().message)}`);

// amend() without `updateRef` only writes a new commit object; the reference is
// left pointing at the original. This mirrors "amend and move the ref separately".
const amendedDetached = repo.headCommit().amend({
  message: "Amended but detached",
});
console.log(`amend(message)           = ${short(amendedDetached)} (HEAD unchanged below)`);
console.log(`  HEAD still at           = ${short(repo.headCommit().id)}`);

// ---------------------------------------------------------------------------
// 8. Signing and verifying
// ---------------------------------------------------------------------------
console.log("\n=== 8. Signing and verifying ===");

// An unsigned commit carries no signature and `verify()` reports null.
console.log(`signature (unsigned)     = ${repo.headCommit().signature ?? "null"}`);
console.log(`verify() (unsigned)      = ${repo.headCommit().verify() ?? "null"}`);

// `sign()` produces a NEW commit object that carries a signature, using the
// signing key configured for the repository (`user.signingKey`, `gpg.format`,
// ...). It shells out to gpg/ssh-keygen, so it requires a usable key; if none is
// configured this throws, which we surface rather than abort the example.
try {
  const signedCommit = repo.headCommit().sign();
  console.log(`sign()                   = ${short(signedCommit.id)}`);
  console.log(`  signature present       = ${signedCommit.signature ? "yes" : "no"}`);
  const outcome = signedCommit.verify();
  console.log(
    `  verify()                = ${outcome ? `${outcome.status} (${outcome.trustLevel})` : "null"}`,
  );
} catch (e) {
  console.log(
    `sign() skipped: ${(e as Error).message.split("\n")[0]} (configure a signing key to enable)`,
  );
}

// ---------------------------------------------------------------------------
// 9. Trailers and object text round-trip
// ---------------------------------------------------------------------------
console.log("\n=== 9. Trailers and object text ===");

// The first commit carried a `Signed-off-by` trailer; parse and re-render it.
const rootCommit = repo.findCommit(history.at(-1)!.sha);
const trailers = rootCommit.trailers();
console.log(`trailers                 = ${trailers.length}`);
for (const t of trailers) {
  console.log(`  ${formatTrailer(t)}`);
}
console.log(`coAuthors                = ${rootCommit.coAuthors().length}`);
const multi = [
  { token: "Co-authored-by", value: "B <b@e.com>" },
  { token: "Reviewed-by", value: "C <c@e.com>" },
];
console.log(`formatTrailers()         = ${formatTrailers(multi).replace(/\n/g, " | ")}`);

// toObjectText() serialises the commit to its canonical raw git object text —
// the exact bytes `git cat-file commit <oid>` would emit.
const text = rootCommit.toObjectText();
console.log(`toObjectText() has tree  = ${text.includes("tree ")}`);
console.log(`toObjectText() has msg   = ${text.includes("Initial commit")}`);

// ---------------------------------------------------------------------------
// 10. Raw object bytes (toBuffer) — the byte-for-byte counterpart of
// toObjectText(): the exact bytes `git cat-file commit <oid>` emits.
// ---------------------------------------------------------------------------
console.log("\n=== 10. Raw object bytes (toBuffer) ===");
const buf = rootCommit.toBuffer();
console.log(`toBuffer() is a Buffer       = ${Buffer.isBuffer(buf)}`);
console.log(`toBuffer().length            = ${buf.length}`);
console.log(
  `toBuffer() == toObjectText()  = ${buf.toString("utf8") === rootCommit.toObjectText()}`,
);

rmSync(base, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
console.log("\ncommit example complete.");
