import { describe, expect, test } from "vite-plus/test";
import { $, exec, execSync, execFile, execFileSync, spawn, spawnSync } from "./index.ts";

describe("Bun-compatible Shell ($)", () => {
  test("echo outputs text", async () => {
    const text = await $`echo "Hello World!"`.text();
    expect(text.trim()).toBe("Hello World!");
  });

  test("pwd builtin returns directory", async () => {
    const pwd = await $`pwd`.text();
    expect(pwd.trim().length).toBeGreaterThan(0);
  });

  test("builtins: false fails when nothrow is false, passes when nothrow is set", async () => {
    await expect($`false`).rejects.toThrow();

    const res = await $`false`.nothrow();
    expect(res.exitCode).toBe(1);
  });

  test("pipeline: echo | cat", async () => {
    const text = await $`echo "pipeline works" | cat`.text();
    expect(text.trim()).toBe("pipeline works");
  });

  test("output as json", async () => {
    const data = await $`echo '{"foo": "bar", "num": 42}'`.json();
    expect(data).toEqual({ foo: "bar", num: 42 });
  });

  test("output lines iterator", async () => {
    const lines: string[] = [];
    for await (const line of $`seq 1 3`.lines()) {
      lines.push(line);
    }
    expect(lines).toEqual(["1", "2", "3"]);
  });

  test("$.braces expansion via fusevm", () => {
    const expanded = $.braces("echo {1,2,3}");
    expect(expanded).toEqual(["echo 1", "echo 2", "echo 3"]);
  });

  test("$.escape logic", () => {
    const escaped = $.escape('$(foo) `bar` "baz"');
    expect(escaped).toContain("\\$(foo)");
  });

  test("environment variable override", async () => {
    const text = await $`FOO=bar echo test`.text();
    expect(text.trim()).toBe("test");
  });

  test("builtin command 'repo'", async () => {
    const help = await $`repo`.text();
    expect(help).toContain("Usage: repo <subcommand>");

    const isRepo = await $`repo is-repo`.text();
    expect(isRepo.trim()).toBe("true");

    const status = await $`repo status`.text();
    expect(status).toContain("git_dir:");
  });
});

describe("node:child_process alias APIs", () => {
  test("exec runs shell command", async () => {
    const res = await exec("echo child_process_exec");
    expect(res.stdout.toString().trim()).toBe("child_process_exec");
  });

  test("execSync returns stdout Buffer", () => {
    const buf = execSync("echo sync_exec");
    expect(buf.toString().trim()).toBe("sync_exec");
  });

  test("execFile executes binary", async () => {
    const res = await execFile("echo", ["file_arg"]);
    expect(res.stdout.toString().trim()).toBe("file_arg");
  });

  test("execFileSync executes binary synchronously", () => {
    const buf = execFileSync("echo", ["sync_file"]);
    expect(buf.toString().trim()).toBe("sync_file");
  });

  test("spawn / spawnSync", async () => {
    const asyncRes = await spawn("echo", ["spawned"]);
    expect(asyncRes.stdout.toString().trim()).toBe("spawned");

    const syncRes = spawnSync("echo", ["spawned_sync"]);
    expect(syncRes.stdout.toString().trim()).toBe("spawned_sync");
  });
});
