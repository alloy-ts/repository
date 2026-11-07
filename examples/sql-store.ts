import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { MemoryObjectStore, SqlObjectStore } from "../workspaces/repository/src/main.ts";

const short = (id: string) => id.slice(0, 8);

function main() {
  console.log("=== SqlObjectStore (in-memory backend) ===");
  const memDb = new SqlObjectStore(":memory:", "sha1");
  console.log(`opened SqlObjectStore at '${memDb.path}' (hash: ${memDb.objectHash})`);

  // Write a blob object
  const blobContent = Buffer.from("Hello from SQLite-backed git object store!\n");
  const blobId = memDb.writeObject("blob", blobContent);
  console.log(`writeObject("blob") -> id=${short(blobId)} (${blobId})`);
  console.log(`contains(${short(blobId)}) -> ${memDb.contains(blobId)}`);

  // Read header and content
  const header = memDb.tryHeader(blobId);
  console.log(`tryHeader -> kind=${header?.kind} size=${header?.size}B`);
  const obj = memDb.tryFind(blobId);
  console.log(`tryFind -> kind=${obj?.kind} data=${JSON.stringify(obj?.data.toString())}`);

  // Prefix lookup
  const prefixResult = memDb.lookupPrefix(blobId.slice(0, 7));
  console.log(
    `lookupPrefix("${blobId.slice(0, 7)}") -> found=${prefixResult.found} ambiguous=${prefixResult.ambiguous} id=${prefixResult.id ? short(prefixResult.id) : null}`,
  );

  // Streaming reader
  console.log("\n=== Streaming Reader ===");
  const reader = memDb.reader(blobId);
  console.log(`reader -> kind=${reader.kind} size=${reader.size}B`);
  const chunks: Buffer[] = [];
  let chunk = reader.read(16);
  while (chunk.length > 0) {
    chunks.push(Buffer.from(chunk));
    chunk = reader.read(16);
  }
  console.log(
    `streamed in ${chunks.length} chunk(s): "${Buffer.concat(chunks).toString().trim()}"`,
  );

  // Streaming writer
  console.log("\n=== Streaming Writer ===");
  const writerContent = Buffer.from("Streaming writer content in SQLite store\n");
  const writer = memDb.writer("blob", writerContent.length);
  writer.write(writerContent.subarray(0, 20));
  writer.write(writerContent.subarray(20));
  const streamedId = writer.finalise();
  console.log(`writer.finalise() -> id=${short(streamedId)}`);
  console.log(`store size now -> ${memDb.size()} object(s)`);

  // Object copying
  console.log("\n=== Copy Object ===");
  const copyTargetId = "1111111111111111111111111111111111111111";
  memDb.copy(blobId, copyTargetId);
  console.log(`copied ${short(blobId)} to ${short(copyTargetId)}`);
  console.log(`contains(${short(copyTargetId)}) -> ${memDb.contains(copyTargetId)}`);
  console.log(`store entries: ${memDb.entries().map(short).join(", ")}`);

  // Packfile ingestion
  console.log("\n=== Packfile Interoperability ===");
  const memorySource = new MemoryObjectStore();
  const packBlobA = memorySource.writeObject("blob", Buffer.from("pack item alpha\n"));
  const packBlobB = memorySource.writeObject("blob", Buffer.from("pack item beta\n"));
  const pack = memorySource.dump();
  console.log(`generated packfile (${pack.length} bytes) from MemoryObjectStore`);

  memDb.importStream(pack);
  console.log(`importStream: contains(${short(packBlobA)}) -> ${memDb.contains(packBlobA)}`);
  console.log(`importStream: contains(${short(packBlobB)}) -> ${memDb.contains(packBlobB)}`);
  console.log(`total objects in SQLite store -> ${memDb.size()}`);

  // Packfile dump from SqlObjectStore
  const exportedPack = memDb.dump();
  console.log(`memDb.dump() exported packfile (${exportedPack.length} bytes)`);

  // File-backed SQLite database persistence
  console.log("\n=== File-Backed SQLite Database Persistence ===");
  const tmpDir = mkdtempSync(join(tmpdir(), "example-sql-store-"));
  const sqliteFile = join(tmpDir, "objects.sqlite");

  try {
    const fileDb1 = new SqlObjectStore(sqliteFile, "sha1");
    const persistentId = fileDb1.writeObject("blob", Buffer.from("persisted across connections\n"));
    console.log(`created file-backed DB at ${sqliteFile}`);
    console.log(`wrote object ${short(persistentId)}`);

    // Reopen from a separate SqlObjectStore instance
    const fileDb2 = new SqlObjectStore(sqliteFile, "sha1");
    console.log(
      `reopened DB: contains(${short(persistentId)}) -> ${fileDb2.contains(persistentId)}`,
    );
    const readBack = fileDb2.tryFind(persistentId);
    console.log(`readBack content -> ${JSON.stringify(readBack?.data.toString())}`);
  } finally {
    rmSync(tmpDir, { recursive: true, force: true });
  }

  // SHA-256 demonstration
  console.log("\n=== SHA-256 Object Store ===");
  const sha256Db = new SqlObjectStore(":memory:", "sha256");
  const sha256Id = sha256Db.writeObject("blob", Buffer.from("sha256 payload\n"));
  console.log(`SHA-256 object hash: ${sha256Db.objectHash}`);
  console.log(`SHA-256 object id length: ${sha256Id.length} chars (${short(sha256Id)}...)`);

  console.log("\nSqlObjectStore example completed successfully!");
}

main();
