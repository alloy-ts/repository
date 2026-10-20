import { Repository } from "../src/main.ts";

const repoPath = process.argv[2] ?? process.cwd();

function main() {
  console.log(`Opening repository at: ${repoPath}`);
  const repo = Repository.open(repoPath);

  // 1. Commit object inspection
  const headCommit = repo.headCommit();
  console.log("\n--- Commit Inspection ---");
  console.log(`Commit ID:      ${headCommit.id}`);
  console.log(`Short ID:       ${headCommit.shortId()}`);
  console.log(`Tree ID:        ${headCommit.treeId}`);
  console.log(`Parents:        ${headCommit.parentIds.join(", ")}`);
  console.log(`Is Signed:      ${headCommit.isSigned}`);
  console.log(`Author:         ${headCommit.author.name} <${headCommit.author.email}>`);
  console.log(`Committer:      ${headCommit.committer.name} <${headCommit.committer.email}>`);
  console.log(`Timestamp:      ${new Date(headCommit.time * 1000).toISOString()}`);
  console.log(`Message:        ${JSON.stringify(headCommit.message)}`);

  // 2. Tree and Blob inspection
  console.log("\n--- Tree & Blob Inspection ---");
  const tree = repo.findTree(headCommit.treeId);
  console.log(`Root tree entries count: ${tree.entries.length}`);

  const readmeEntry = tree.entry("ReadMe.md") ?? tree.entries[0];
  if (readmeEntry && readmeEntry.mode !== "040000") {
    const blob = repo.findBlob(readmeEntry.id);
    console.log(`Entry path:     ${readmeEntry.mode} ${readmeEntry.id}`);
    console.log(`Blob size:      ${blob.size} bytes`);
    const content = blob.content().toString("utf-8");
    const preview = content.length > 80 ? content.slice(0, 80) + "..." : content;
    console.log(`Blob preview:   ${JSON.stringify(preview)}`);
  }

  // 3. Generic GitObject and Peel helper
  console.log("\n--- GitObject & Peel ---");
  const gitObj = repo.findObject(headCommit.id);
  console.log(`GitObject ID:   ${gitObj.id}`);
  console.log(`GitObject Kind: ${gitObj.kind}`);

  const peeledTree = gitObj.peel("tree");
  console.log(`Peeled to tree: ${peeledTree.id} (kind: ${peeledTree.kind})`);

  // 4. Annotated Tag inspection (if any)
  console.log("\n--- Tag Inspection ---");
  const tagRef = repo.references().find((r) => r.name.startsWith("refs/tags/"));
  if (tagRef && tagRef.target) {
    try {
      const tag = repo.findTag(tagRef.target);
      console.log(`Tag Name:       ${tag.name}`);
      console.log(`Tag ID:         ${tag.id}`);
      console.log(`Target ID:      ${tag.targetId}`);
      console.log(`Target Kind:    ${tag.targetKind}`);
      console.log(`Tag Message:    ${JSON.stringify(tag.message)}`);
      if (tag.tagger) {
        console.log(`Tagger:         ${tag.tagger.name} <${tag.tagger.email}>`);
      }
    } catch {
      console.log(`Tag ref ${tagRef.name} is lightweight (points directly to ${tagRef.target}).`);
    }
  } else {
    console.log("No tag references found in this repository.");
  }
}

main();
