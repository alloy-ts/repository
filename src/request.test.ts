import { describe, expect, test } from "vite-plus/test";
import { GitRequestClient, ObjectStoreClient, ProtocolCommand, gitServiceUrl } from "./main.ts";

describe("Request and ObjectStoreClient JS bindings", () => {
  test("ObjectStoreClient manages HTTP and SSH options", () => {
    const client = new ObjectStoreClient({
      allowHttp: true,
      userAgent: "test-agent/1.0",
      ssh: {
        host: "git.example.com",
        port: 2222,
        user: "git",
      },
    });

    expect(client.getConfigValue("allow_http")).toBe("true");
    expect(client.getConfigValue("user_agent")).toBe("test-agent/1.0");

    const ssh = client.getSshConfig();
    expect(ssh).not.toBeNull();
    expect(ssh?.host).toBe("git.example.com");
    expect(ssh?.port).toBe(2222);

    const prefix = client.getSshCommandPrefix();
    expect(prefix).toEqual(["ssh", "-p", "2222", "git@git.example.com"]);

    expect(client.buildHttpClientOk()).toBe(true);
  });

  test("gitServiceUrl constructs V2 protocol endpoints", () => {
    const lsRefsUrl = gitServiceUrl("https://github.com/org/repo", ProtocolCommand.LsRefs);
    expect(lsRefsUrl).toBe("https://github.com/org/repo/info/refs?service=git-upload-pack");

    const fetchUrl = gitServiceUrl("https://github.com/org/repo/", ProtocolCommand.Fetch);
    expect(fetchUrl).toBe("https://github.com/org/repo/git-upload-pack");
  });

  test("GitRequestClient creates instances", () => {
    const client = new GitRequestClient("custom-ua");
    expect(client).toBeDefined();
  });
});
