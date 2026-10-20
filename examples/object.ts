import { Repository } from "../src/main.ts";

const repoPath = process.argv[2] ?? process.cwd();

const short = (id: string) => id.slice(0, 8);

function main() {
  console.log(`Opening repository at: ${repoPath}`);
  const repo = Repository.open(repoPath);
  const head = repo.headCommit();

  // ---------------------------------------------------------------------------
  // Blobs: file contents, stored verbatim.
  // ---------------------------------------------------------------------------
  console.log("\n=== Blob ===");
  const tree = repo.findTree(head.treeId);
  const firstFile = tree.entries.find((e) => e.mode === "100644");
  if (firstFile) {
    const blob = repo.findBlob(firstFile.id);
    console.log(`${firstFile.filename}: ${blob.content().length} byte(s) at ${short(blob.id)}`);
  }
  const blobId = repo.writeBlob(Buffer.from("hello from examples/object.ts\n"));
  console.log(`writeBlob() -> ${short(blobId)}`);
  console.log(
    `writeBlobStream() -> ${short(repo.writeBlobStream(Buffer.from("streamed blob\n")))}`,
  );

  // ---------------------------------------------------------------------------
  // Trees: directory snapshots. Entries point at blobs, subtrees, symlinks, or
  // submodules. writeTree() builds one from explicit entries; editTree() applies
  // upserts/removals on top of a base tree and auto-creates/prunes subtrees.
  // ---------------------------------------------------------------------------
  console.log("\n=== Tree ===");
  console.log(`HEAD tree ${short(tree.id)} has ${tree.entries.length} direct entr(ies):`);
  for (const e of tree.entries.slice(0, 5)) {
    console.log(`  ${e.mode} ${e.id.slice(0, 8)} ${e.filename}`);
  }
  const byName = tree.entry(firstFile?.filename ?? "nonexistent");
  console.log(`tree.entry("${firstFile?.filename}") -> ${byName ? short(byName.id) : "null"}`);

  // entryByPath() follows nested trees and returns an Entry with traversal helpers.
  const dir = tree.entries.find((e) => e.mode === "040000");
  if (dir) {
    const sub = tree.entryByPath(`${dir.filename}`);
    const nested = sub?.asTree();
    console.log(
      `entryByPath("${dir.filename}") isTree=${sub?.isTree()} -> subtree with ${
        nested?.entries.length ?? 0
      } entr(ies)`,
    );
  }

  // Build a tree from scratch, then edit one on top of HEAD.
  const scratchTree = repo.writeTree([
    {
      path: "greeting.txt",
      mode: "100644",
      id: blobId,
    },
  ]);
  console.log(`writeTree() -> ${short(scratchTree)}; emptyTree() -> ${short(repo.emptyTree().id)}`);
  const editedTree = repo.editTree(head.treeId, [
    { path: "docs/greeting.txt", mode: "100644", id: blobId }, // creates docs/ subtree
    { path: "greeting.txt", id: blobId },
  ]);
  const docs = repo.findTree(editedTree).entryByPath("docs/greeting.txt");
  console.log(
    `editTree() -> ${short(editedTree)}; docs/greeting.txt = ${docs ? short(docs.id) : "null"}`,
  );

  // removeMode semantics: "leaf" refuses trees, "ifLeaf" skips them, default "any" removes.
  const leafRemoved = repo.editTree(scratchTree, [
    { path: "greeting.txt", remove: true, removeMode: "leaf" },
  ]);
  console.log(
    `removeMode "leaf" removed the blob -> tree now has ${
      repo.findTree(leafRemoved).entries.length
    } entr(ies)`,
  );
  try {
    repo.editTree(scratchTree, [
      {
        path: "greeting.txt",
        remove: true,
        removeMode: "bogus",
      },
    ]);
  } catch (e) {
    console.log(`invalid removeMode rejected: ${(e as Error).message}`);
  }

  // ---------------------------------------------------------------------------
  // Commits: read fields, write detached commits (objects only, no refs move).
  // ---------------------------------------------------------------------------
  console.log("\n=== Commit ===");
  console.log(`${short(head.id)} "${head.message.split("\n")[0]}"`);
  console.log(`  tree ${short(head.treeId)}, ${head.parentIds.length} parent(s)`);
  const author = head.author();
  console.log(`  author ${author.name} <${author.email}> at ${author.time}`);
  console.log(`  encoding: ${head.encoding ?? "utf-8 (default)"}`);
  console.log(`  extra headers: ${head.extraHeaders().length}`);
  const commitId = repo.newCommit(
    "examples/object.ts demo commit\n\nWritten to the ODB without moving any reference.\n",
    editedTree,
    [head.id],
  );
  console.log(`newCommit() -> ${short(commitId)}`);
  const writeCommitId = repo.writeCommit({
    tree: scratchTree,
    parents: [head.id],
    message: "writeCommit() demo\n",
  });
  console.log(`writeCommit() -> ${short(writeCommitId)}`);

  // ---------------------------------------------------------------------------
  // Tags: annotated tag objects. writeTag() writes the object only;
  // createTag() would additionally create refs/tags/<name>.
  // ---------------------------------------------------------------------------
  console.log("\n=== Tag ===");
  const tagger = head.author();
  const tagId = repo.writeTag({
    name: "examples-object-demo",
    target: commitId,
    targetKind: "commit",
    message: "Annotated tag written by examples/object.ts\n",
    tagger,
  });
  const tag = repo.findTag(tagId);
  console.log(
    `writeTag() -> ${short(tag.id)} "${tag.name}" -> ${tag.targetKind} ${short(tag.targetId)}`,
  );
  console.log(`  message: "${tag.message.trim().split("\n")[0]}"`);
  console.log(
    `  tagger: ${tag.tagger()?.name ?? "(none)"}, signature: ${tag.signature ? "yes" : "none"}`,
  );

  // ---------------------------------------------------------------------------
  // Raw object store + GitObject downcasts: kinded access to any object.
  // ---------------------------------------------------------------------------
  console.log("\n=== Raw objects ===");
  for (const [label, id] of [
    ["commit", commitId],
    ["tree", editedTree],
    ["blob", blobId],
    ["tag", tagId],
  ] as const) {
    const header = repo.findHeader(id);
    const obj = repo.findObject(id);
    console.log(
      `${label.padEnd(6)} ${short(
        id,
      )} kind=${obj.kind} header=${header.kind}/${header.size}B exists=${repo.objectExists(id)}`,
    );
    const downcast = obj.asCommit()?.id ?? obj.asTree()?.id ?? obj.asBlob()?.id ?? obj.asTag()?.id;
    console.log(`         downcast -> ${short(downcast!)}, data=${obj.data().length}B`);
  }

  // try* variants return null instead of throwing; peel follows tag/commit chains.
  console.log(`tryFindObject(missing) -> ${repo.tryFindObject("0".repeat(40))}`);
  console.log(`tryFindHeader(missing) -> ${repo.tryFindHeader("0".repeat(40))}`);
  console.log(
    `computeHash(blob, data) == blobId -> ${
      repo.computeHash("blob", Buffer.from("hello from examples/object.ts\n")) === blobId
    }`,
  );
  console.log(`peel(tagId, "commit") -> ${short(repo.peel(tagId, "commit"))}`);
  console.log(`GitObject.peelToTree() -> ${short(repo.findObject(tagId).peelToTree().id)}`);

  // Raw round-trip: writeObject(readObject(id)) is content-addressed, so ids match.
  const raw = repo.readObject(tree.id);
  console.log(
    `readObject/writeObject round-trip stable -> ${
      repo.writeObject(raw.kind, raw.data) === tree.id
    }`,
  );
}

main();
