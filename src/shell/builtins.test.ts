import { describe, expect, test } from "vite-plus/test";
import { $ } from "./index.ts";
import * as fs from "node:fs";
import * as path from "node:path";
import * as os from "node:os";

describe("Shell Builtin Commands", () => {
  test("echo: normal and -n flag", async () => {
    const normal = await $`echo hello world`.text();
    expect(normal).toBe("hello world\n");

    const noNewline = await $`echo -n hello`.text();
    expect(noNewline).toBe("hello");
  });

  test("pwd", async () => {
    const cwd = await $`pwd`.text();
    expect(cwd.trim()).toBe(process.cwd());
  });

  test("seq: range and step", async () => {
    const seq1 = await $`seq 3`.text();
    expect(seq1.trim().split("\n")).toEqual(["1", "2", "3"]);

    const seq2 = await $`seq 1 2 5`.text();
    expect(seq2.trim().split("\n")).toEqual(["1", "3", "5"]);
  });

  test("dirname and basename", async () => {
    const dir = await $`dirname /path/to/file.txt`.text();
    expect(dir.trim()).toBe("/path/to");

    const base = await $`basename /path/to/file.txt`.text();
    expect(base.trim()).toBe("file.txt");

    const baseSuffix = await $`basename /path/to/file.txt .txt`.text();
    expect(baseSuffix.trim()).toBe("file");
  });

  test("true and false exit codes", async () => {
    const resTrue = await $`true`.nothrow();
    expect(resTrue.exitCode).toBe(0);

    const resFalse = await $`false`.nothrow();
    expect(resFalse.exitCode).toBe(1);
  });

  test("wc counts lines, words, bytes from stdin or files", async () => {
    const wcStdin = await $`echo "hello world" | wc -w`.text();
    expect(wcStdin.trim()).toBe("2");

    const wcLines = await $`seq 1 5 | wc -l`.text();
    expect(wcLines.trim()).toBe("5");
  });

  test("grep filters lines with case insensitivity and invert match", async () => {
    const matched = await $`echo "apple\nBanana\ncherry" | grep -i "banana"`.text();
    expect(matched.trim()).toBe("Banana");

    const inverted = await $`echo "apple\nBanana\ncherry" | grep -v "a"`.text();
    expect(inverted.trim()).toBe("cherry");
  });

  test("touch, ls, cat, and rm", async () => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "shell-test-"));
    const fileA = path.join(tmpDir, "test.txt");

    // touch
    await $`touch ${fileA}`;
    expect(fs.existsSync(fileA)).toBe(true);

    // echo > file
    await $`echo "contents" > ${fileA}`;

    // cat
    const content = await $`cat ${fileA}`.text();
    expect(content.trim()).toBe("contents");

    // ls
    const list = await $`ls ${tmpDir}`.text();
    expect(list.trim()).toContain("test.txt");

    // rm
    await $`rm ${fileA}`;
    expect(fs.existsSync(fileA)).toBe(false);

    // rm -r
    await $`rm -rf ${tmpDir}`;
    expect(fs.existsSync(tmpDir)).toBe(false);
  });

  test("mkdir and mv", async () => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "shell-mkdir-"));
    const subDir = path.join(tmpDir, "sub");
    const srcFile = path.join(tmpDir, "foo.txt");
    const dstFile = path.join(tmpDir, "bar.txt");

    await $`mkdir -p ${subDir}`;
    expect(fs.existsSync(subDir)).toBe(true);

    await $`touch ${srcFile}`;
    await $`mv ${srcFile} ${dstFile}`;
    expect(fs.existsSync(srcFile)).toBe(false);
    expect(fs.existsSync(dstFile)).toBe(true);

    await $`rm -rf ${tmpDir}`;
  });

  test("which finds system executables", async () => {
    const nodePath = await $`which node`.text();
    expect(nodePath.trim().length).toBeGreaterThan(0);
  });

  test("repo and git builtin", async () => {
    const repoHelp = await $`repo`.text();
    expect(repoHelp).toContain("Usage: repo <subcommand>");

    const gitHelp = await $`git`.text();
    expect(gitHelp).toContain("Usage: repo <subcommand>");

    const isRepo = await $`git is-repo`.text();
    expect(isRepo.trim()).toBe("true");

    const status = await $`git status`.text();
    expect(status).toContain("git_dir:");
  });

  test("curl: missing url and basic handling", async () => {
    const missingUrl = await $`curl`.nothrow();
    expect(missingUrl.exitCode).toBe(2);
    expect(missingUrl.stderr.toString()).toContain("curl: no URL specified");
  });

  test("curl: local HTTP server GET, POST, headers, and output file", async () => {
    const http = await import("node:http");
    let receivedMethod = "";
    let receivedHeaders: Record<string, string | string[] | undefined> = {};
    let receivedBody = "";

    const server = http.createServer((req, res) => {
      receivedMethod = req.method || "";
      receivedHeaders = req.headers;
      let body = "";
      req.on("data", (chunk) => {
        body += chunk;
      });
      req.on("end", () => {
        receivedBody = body;
        if (req.url === "/404") {
          res.writeHead(404, { "Content-Type": "text/plain" });
          res.end("Not Found");
        } else {
          res.writeHead(200, {
            "Content-Type": "application/json",
            "X-Custom-Echo": "foobar",
          });
          res.end(JSON.stringify({ ok: true, echo: body }));
        }
      });
    });

    await new Promise<void>((resolve) => {
      server.listen(0, "127.0.0.1", () => resolve());
    });

    const addr = server.address() as any;
    const port = addr.port;
    const baseUrl = `http://127.0.0.1:${port}`;

    try {
      // 1. GET
      const resGet = await $`curl -s ${baseUrl}/test`.text();
      expect(JSON.parse(resGet)).toEqual({ ok: true, echo: "" });
      expect(receivedMethod).toBe("GET");

      // 2. POST with -d and -H
      const resPost = await $`curl -s -X POST -H "X-Client: test-client" -d "hello=world" ${baseUrl}/post`.text();
      expect(JSON.parse(resPost)).toEqual({ ok: true, echo: "hello=world" });
      expect(receivedMethod).toBe("POST");
      expect(receivedHeaders["x-client"]).toBe("test-client");

      // 3. Output to file (-o)
      const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "shell-curl-"));
      const outFile = path.join(tmpDir, "out.json");
      await $`curl -s -o ${outFile} ${baseUrl}/file`;
      expect(fs.existsSync(outFile)).toBe(true);
      const fileData = fs.readFileSync(outFile, "utf-8");
      expect(JSON.parse(fileData)).toEqual({ ok: true, echo: "" });
      fs.rmSync(tmpDir, { recursive: true, force: true });

      // 4. -f flag on 404
      const res404 = await $`curl -f -s ${baseUrl}/404`.nothrow();
      expect(res404.exitCode).toBe(22);
    } finally {
      await new Promise<void>((resolve) => server.close(() => resolve()));
    }
  });
});
