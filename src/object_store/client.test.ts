import { describe, expect, test } from "vite-plus/test";
import { ObjectStoreClient, SshTransport } from "./main.ts";

describe("SshTransport (src/protocol.rs)", () => {
  test("SshTransport can be instantiated", () => {
    const transport = new SshTransport("git", 22, "/path/to/id_rsa");
    expect(transport).toBeDefined();
  });
});

describe("ObjectStoreClient (src/object_store/client.rs)", () => {
  test("ObjectStoreClient can be instantiated with options", () => {
    const client = new ObjectStoreClient("https://example.com/store", {
      headers: { Authorization: "Bearer token123" },
      timeoutMs: 5000,
    });
    expect(client).toBeDefined();
  });
});
