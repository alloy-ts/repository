import * as dist from "../../../dist/index.js";

// Mount git object types under Object class
if (dist.Object && dist.object) {
  (dist.Object as any).Blob = dist.object.Blob;
  (dist.Object as any).Commit = dist.object.Commit;
  (dist.Object as any).Tree = dist.object.Tree;
  (dist.Object as any).Tag = dist.object.Tag;
}

export * from "../../../dist/index.js";
