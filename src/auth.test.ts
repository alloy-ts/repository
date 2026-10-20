import { execSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";

import { Auth, Repository } from "./main.ts";

let fixture: string;

// Same style as the submodule tests: let the shell resolve `git`.
function git(args: string, cwd: string) {
  execSync(`git -c protocol.file.allow=always ${args}`, {
    cwd,
    stdio: "ignore",
  });
}

beforeAll(() => {
  fixture = mkdtempSync(join(tmpdir(), "repository-auth-"));
});

afterAll(() => {
  rmSync(fixture, { recursive: true, force: true });
});

describe("Auth config translation", () => {
  test("defaults set GIT_TERMINAL_PROMPT=0 and no other env", () => {
    const auth = new Auth();
    expect(auth.gitEnvMap()).toEqual({ GIT_TERMINAL_PROMPT: "0" });
    expect(auth.gitConfig()).toEqual([]);
  });

  test("plaintext credentials become GIT_AUTH_* env vars + a credential.helper", () => {
    const auth = new Auth();
    auth.addPlaintextCredentials("github.com", "me", "s3cr3t");
    const env = auth.gitEnvMap();
    expect(env.GIT_AUTH_0_U).toBe("me");
    expect(env.GIT_AUTH_0_P).toBe("s3cr3t");
    expect(env.GIT_TERMINAL_PROMPT).toBe("0");

    const cfg = auth.gitConfig();
    expect(cfg.some((c) => c.startsWith("credential.helper=!f()"))).toBe(true);
    // The helper must reference the secret env vars (not embed them in the command).
    expect(cfg.some((c) => c.includes("$GIT_AUTH_0_U"))).toBe(true);
    expect(cfg.some((c) => c.includes("$GIT_AUTH_0_P"))).toBe(true);
  });

  test("multiple credentials are indexed", () => {
    const auth = new Auth();
    auth.addPlaintextCredentials("github.com", "a", "pa");
    auth.addPlaintextCredentials("*", "b", "pb");
    const env = auth.gitEnvMap();
    expect(env.GIT_AUTH_0_U).toBe("a");
    expect(env.GIT_AUTH_1_U).toBe("b");
    expect(env.GIT_AUTH_1_P).toBe("pb");
  });

  test("SSH key becomes GIT_SSH_COMMAND with -i and IdentitiesOnly", () => {
    const auth = new Auth();
    auth.addSshKeyFromFile("/tmp/mykey");
    const sshCmd = auth.gitEnvMap().GIT_SSH_COMMAND;
    expect(sshCmd).toContain("-i /tmp/mykey");
    expect(sshCmd).toContain("IdentitiesOnly=yes");
  });

  test("addDefaultSshKeys does not break translation (no env side effects in-process)", () => {
    const auth = new Auth();
    auth.addDefaultSshKeys();
    // No assertion on specific keys (they depend on the environment); just ensure it doesn't throw
    // and still emits the terminal-prompt guard.
    expect(auth.gitEnvMap().GIT_TERMINAL_PROMPT).toBe("0");
  });

  test("sshAgent:false clears SSH_AUTH_SOCK", () => {
    const auth = new Auth({ sshAgent: false });
    expect(auth.gitEnvMap().SSH_AUTH_SOCK).toBe("");
  });

  test("credentialHelper:false emits an empty credential.helper override", () => {
    const auth = new Auth({ credentialHelper: false });
    expect(auth.gitConfig().some((c) => c === "credential.helper=")).toBe(true);
  });

  test("builder methods accumulate onto defaults", () => {
    const auth = new Auth({
      sshAgent: false,
      credentialHelper: false,
    });
    auth.addPlaintextCredentials("*", "u", "p");
    auth.addSshKeyFromFile("/tmp/k2");
    auth.addUsername("github.com", "ghuser");

    const env = auth.gitEnvMap();
    expect(env.SSH_AUTH_SOCK).toBe("");
    expect(env.GIT_AUTH_0_U).toBe("u");
    expect(env.GIT_SSH_COMMAND).toContain("-i /tmp/k2");

    const cfg = auth.gitConfig();
    expect(cfg.some((c) => c === "credential.helper=")).toBe(true);
    expect(cfg.some((c) => c.startsWith("credential.helper=!f()"))).toBe(true);
  });
});

describe("Auth integration (clone)", () => {
  test("passing auth to Repository.clone threads through and clones a local repo", async () => {
    const src = join(fixture, "src");
    const dest = join(fixture, "dest");
    mkdirSync(src, { recursive: true });
    git("init --initial-branch=main", src);
    git("config user.email a@e.com", src);
    git("config user.name A", src);
    git("config commit.gpgsign false", src);
    writeFileSync(join(src, "a.txt"), "hello\n");
    git("add a.txt", src);
    git("commit -m init", src);

    // auth present → routes through the git subprocess with the auth env/config applied.
    const auth = new Auth({
      credentials: [{ domain: "*", username: "u", password: "p" }],
    });
    const repo = await Repository.clone(src, dest, { auth });
    expect(repo).toBeDefined();
    expect(existsSync(join(dest, "a.txt"))).toBe(true);
  });
});
