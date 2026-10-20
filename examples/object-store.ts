import { ObjectDatabase, ObjectHandle, ObjectStore } from "../src/main.ts";
import { execSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const repoPath = process.argv[2] ?? process.cwd();
const short = (id: string) => id.slice(0, 8);

function git(args: string): string {
  return execSync(`git ${args}`, { cwd: repoPath, encoding: "utf8" }).trim();
}

function main() {
  // -------------------------------------------------------------------------
  // ObjectStore: loose-only, content-addressed scratch storage.
  // -------------------------------------------------------------------------
  console.log("=== ObjectStore (loose-only) ===");
  const tmp = mkdtempSync(join(tmpdir(), "examples-store-"));
  const looseDir = join(tmp, "objects");
  mkdirSync(looseDir, { recursive: true });
  const loose = new ObjectStore(looseDir);

  const data = Buffer.from("hello from examples/store.ts\n");
  const blobId = loose.writeObject("blob", data);
  console.log(`writeObject("blob") -> ${short(blobId)}`);
  console.log(`contains(${short(blobId)}) -> ${loose.contains(blobId)}`);
  console.log(`objectPath(${short(blobId)}) -> ${loose.objectPath(blobId)}`);

  const header = loose.tryHeader(blobId);
  console.log(`tryHeader -> kind=${header!.kind} size=${header!.size}B`);
  const body = loose.tryFind(blobId);
  console.log(`tryFind -> kind=${body!.kind} data=${JSON.stringify(body!.data.toString())}`);
  console.log(`tryFind(missing) -> ${loose.tryFind("0".repeat(40))}`);

  const lookup = loose.lookupPrefix(blobId.slice(0, 12));
  console.log(
    `lookupPrefix(12) -> found=${lookup.found} id=${lookup.id ? short(lookup.id) : null}`,
  );
  console.log(`path getter -> ${loose.path}`);
  console.log(`objectHash getter -> ${loose.objectHash}`);

  // -------------------------------------------------------------------------
  // ObjectDatabase: loose + packs + alternates, with optional caches.
  // -------------------------------------------------------------------------
  console.log("\n=== ObjectDatabase (pack-aware) ===");
  let objectsDir: string;
  let headId: string;
  try {
    objectsDir = join(repoPath, git("rev-parse --git-common-dir"), "objects");
    headId = git("rev-parse HEAD");
  } catch {
    console.log("Not a git repository — skipping the pack-aware demonstration.");
    rmSync(tmp, { recursive: true, force: true });
    return;
  }

  const db = new ObjectDatabase(objectsDir);
  console.log(`opened ODB at ${db.path} (hash ${db.objectHash})`);
  console.log(`contains(HEAD) -> ${db.contains(headId)}`);
  console.log(`contains(missing) -> ${db.contains("0".repeat(40))}`);

  const commit = db.tryFind(headId);
  const commitHeader = db.tryHeader(headId);
  console.log(`tryFind(HEAD) -> kind=${commit!.kind} data=${commit!.data.length}B`);
  console.log(`tryHeader(HEAD) -> kind=${commitHeader!.kind} size=${commitHeader!.size}B`);
  console.log(`  body length == header size -> ${commit!.data.length === commitHeader!.size}`);

  const headLookup = db.lookupPrefix(headId);
  console.log(
    `lookupPrefix(HEAD) -> found=${headLookup.found} id=${
      headLookup.id ? short(headLookup.id) : null
    }`,
  );

  // Cache toggling.
  console.log(`\nhas_pack_cache=${db.hasPackCache()} has_object_cache=${db.has_object_cache()}`);
  db.setPackCache("none");
  db.setObjectCache("none");
  console.log(
    `after set_*_cache: hasPackCache=${db.hasPackCache()} has_object_cache=${db.has_object_cache()}`,
  );
  db.unsetPackCache();
  db.unsetObjectCache();
  console.log(
    `after unset_*_cache: hasPackCache=${db.hasPackCache()} has_object_cache=${db.has_object_cache()}`,
  );

  try {
    db.setPackCache("lru");
  } catch (e) {
    console.log(`setPackCache("lru") rejected: ${(e as Error).message}`);
  }

  // intoArc() clones the (already Arc-backed) database; intoInner() drops the cache.
  const cloned = db.intoArc();
  console.log(`intoArc() instanceof ObjectDatabase -> ${cloned instanceof ObjectDatabase}`);
  console.log(`intoArc().contains(HEAD) -> ${cloned.contains(headId)}`);

  const handle: ObjectHandle = db.intoInner();
  console.log(`\ninto_inner() instanceof ObjectHandle -> ${handle instanceof ObjectHandle}`);
  console.log(`handle.contains(HEAD) -> ${handle.contains(headId)}`);
  console.log(`handle.tryHeader(HEAD) -> kind=${handle.tryHeader(headId)!.kind}`);
  console.log(`handle.lookupPrefix(HEAD) -> found=${handle.lookupPrefix(headId).found}`);

  const records = handle.structure();
  console.log(`structure() -> ${records.length} record(s):`);
  for (const r of records) {
    const detail =
      r.numObjects !== undefined ? ` (${r.numObjects} loose)` : r.state ? ` (${r.state})` : "";
    console.log(`  ${r.kind}${r.path ? ` ${short(r.path)}` : ""}${detail}`);
  }
  console.log(`alternate_db_paths() -> ${JSON.stringify(handle.alternate_db_paths())}`);

  rmSync(tmp, { recursive: true, force: true });
}

main();
