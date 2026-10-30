import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import os from "node:os";

import {
  ArrowObjectStore,
} from "../../index.js";

test("ArrowObjectStore from_memory and loose object write/read/contains", () => {
  const store = ArrowObjectStore.fromMemory("sha1", undefined);
  assert.equal(store.objectHash, "sha1");
  assert.equal(store.objectFormat, "sha1");

  const data = Buffer.from("hello world\n");
  const oid = store.writeObject("blob", data);
  assert.equal(typeof oid, "string");
  assert.equal(oid.length, 40);

  assert.equal(store.contains(oid), true);
  assert.equal(store.contains("0000000000000000000000000000000000000000"), false);

  const found = store.tryFind(oid);
  assert.notEqual(found, null);
  assert.equal(found?.kind, "blob");
  assert.equal(found?.data.toString("utf8"), "hello world\n");

  const header = store.tryHeader(oid);
  assert.notEqual(header, null);
  assert.equal(header?.kind, "blob");
  assert.equal(header?.size, data.length);
});

test("ArrowObjectStore list, listWithDelimiter, and listWithOffset", () => {
  const store = ArrowObjectStore.fromMemory("sha1", undefined);

  const data1 = Buffer.from("content 1");
  const data2 = Buffer.from("content 2");

  const oid1 = store.writeObject("blob", data1);
  const oid2 = store.writeObject("blob", data2);

  const allObjects = store.list(undefined);
  assert.ok(allObjects.length >= 2);

  const path1 = store.objectPath(oid1);
  const path2 = store.objectPath(oid2);
  const locations = allObjects.map((m) => m.location);
  assert.ok(locations.includes(path1));
  assert.ok(locations.includes(path2));

  const listRes = store.listWithDelimiter(undefined);
  assert.ok(Array.isArray(listRes.commonPrefixes));
  assert.ok(Array.isArray(listRes.objects));

  const sortedPaths = [path1, path2].sort();
  const offsetResults = store.listWithOffset(undefined, sortedPaths[0]);
  const offsetLocations = offsetResults.map((m) => m.location);
  assert.ok(offsetLocations.includes(sortedPaths[1]));
  assert.ok(!offsetLocations.includes(sortedPaths[0]));
});

test("ArrowObjectStore file system backend constructor", () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "arrow-odb-"));
  try {
    const store = new ArrowObjectStore(tmpDir, "sha1", "objects");
    assert.equal(store.objectHash, "sha1");

    const data = Buffer.from("fs backed arrow store data\n");
    const oid = store.writeObject("blob", data);

    assert.equal(store.contains(oid), true);
    const found = store.tryFind(oid);
    assert.equal(found?.data.toString("utf8"), "fs backed arrow store data\n");

    const listed = store.list(undefined);
    assert.ok(listed.length >= 1);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});
