import * as dist from "../../../dist/index.js";

// Mount git object types under Object class
if (dist.Object && dist.object) {
  (dist.Object as any).Blob = dist.object.Blob;
  (dist.Object as any).Commit = dist.object.Commit;
  (dist.Object as any).Tree = dist.object.Tree;
  (dist.Object as any).Tag = dist.object.Tag;
}


// Enhance File and Blob .stream() to return standard ReadableStream
if (dist.File && typeof ReadableStream !== "undefined") {
  const origFileStream = dist.File.prototype.stream;
  dist.File.prototype.stream = function () {
    const file = this;
    return new ReadableStream({
      async start(controller) {
        try {
          const buf = await file.bytes();
          controller.enqueue(buf);
          controller.close();
        } catch (err) {
          controller.error(err);
        }
      },
    });
  };
}

if (dist.Blob && typeof ReadableStream !== "undefined") {
  const origBlobStream = dist.Blob.prototype.stream;
  dist.Blob.prototype.stream = function () {
    const blob = this;
    return new ReadableStream({
      async start(controller) {
        try {
          const buf = await blob.bytes();
          controller.enqueue(buf);
          controller.close();
        } catch (err) {
          controller.error(err);
        }
      },
    });
  };
}

export * from "../../../dist/index.js";
