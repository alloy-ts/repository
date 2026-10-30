import { describe, expect, test } from "vite-plus/test";
import { ObjectStoreClient } from "../main.ts";

describe("ObjectStoreClient (src/object_store/client.rs)", () => {
  test("creates ObjectStoreClient with options and reads config values", () => {
    const client = new ObjectStoreClient({
      allowHttp: true,
      allowInvalidCertificates: true,
      userAgent: "custom-agent/1.0",
      connectTimeoutMs: 5000,
      transportProtocol: "http",
      sshUser: "git",
      sshKeyPath: "/id_ed25519",
    });

    expect(client.getConfigValue("allow_http")).toBe("true");
    expect(client.getConfigValue("allow_invalid_certificates")).toBe("true");
    expect(client.getConfigValue("user_agent")).toBe("custom-agent/1.0");
    expect(client.getConfigValue("connect_timeout")).toBe("5000ms");
    expect(client.getConfigValue("ssh_user")).toBe("git");
    expect(client.getConfigValue("ssh_key_path")).toBe("/id_ed25519");
    expect(client.transportProtocol).toBe("http");
  });

  test("updates config using setConfigValue", () => {
    const client = new ObjectStoreClient();
    expect(client.getConfigValue("user_agent")).toBeNull();
    client.setConfigValue("user_agent", "new-agent/2.0");
    expect(client.getConfigValue("user_agent")).toBe("new-agent/2.0");
  });

  test("connectHttp returns false for invalid/unreachable url", async () => {
    const client = new ObjectStoreClient({ connectTimeoutMs: 1000, timeoutMs: 1000 });
    const ok = await client.connectHttp("http://127.0.0.1:59999/nonexistent");
    expect(ok).toBe(false);
  });

  test("connectSsh returns false for unreachable SSH target", async () => {
    const client = new ObjectStoreClient({ sshUser: "invalid" });
    const ok = await client.connectSsh("127.0.0.1", "invalid", "/nonexistent_key");
    expect(ok).toBe(false);
  });
});
