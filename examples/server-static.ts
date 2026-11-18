/** Run with `node --import @oxc-node/core/register examples/server-static.ts [directory]`. */
import { realpath, stat } from "node:fs/promises";
import { realpathSync } from "node:fs";
import { extname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { File, Response, Server } from "../sources/repository/src/main.ts";

const contentTypes: Record<string, string> = {
  ".html": "text/html; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".txt": "text/plain; charset=utf-8",
  ".json": "application/json",
  ".yaml": "application/yaml",
  ".yml": "application/yaml",
  ".svg": "image/svg+xml",
  ".png": "image/png",
};

export function createStaticServer(
  directory: string | URL,
  options: { host?: string; port?: number } = {},
): Server {
  const root = realpathSync(directory instanceof URL ? fileURLToPath(directory) : directory);
  return new Server({
    host: options.host ?? "127.0.0.1",
    port: options.port ?? 8080,
    async fetch(request) {
      if (request.method !== "GET" && request.method !== "HEAD") {
        return new Response("Method Not Allowed", {
          status: 405,
          headers: { allow: "GET, HEAD" },
        });
      }
      let pathname: string;
      try {
        pathname = decodeURIComponent(new URL(request.url).pathname);
      } catch {
        return new Response("Bad Request", { status: 400 });
      }
      if (pathname.includes("\0")) return new Response("Bad Request", { status: 400 });

      let path: string;
      try {
        path = await realpath(resolve(root, `.${pathname}`));
        if ((await stat(path)).isDirectory()) path = await realpath(join(path, "index.html"));
      } catch {
        return new Response("Not Found", { status: 404 });
      }
      const withinRoot = relative(root, path);
      if (withinRoot === ".." || withinRoot.startsWith(`..${sep}`) || isAbsolute(withinRoot)) {
        return new Response("Forbidden", { status: 403 });
      }
      if (!(await stat(path)).isFile()) return new Response("Not Found", { status: 404 });

      const file = new File(path, {
        type: contentTypes[extname(path).toLowerCase()] ?? "application/octet-stream",
      });
      return new Response(file.stream(), { headers: { "content-type": file.type } });
    },
  });
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const directory = process.argv[2] ?? new URL("./static/", import.meta.url);
  const port = Number(process.env["PORT"] ?? 8080);
  if (!Number.isInteger(port) || port < 0 || port > 65535) {
    throw new Error("PORT must be an integer from 0 to 65535");
  }
  const server = createStaticServer(directory, {
    host: process.env["HOST"],
    port,
  });
  const boundPort = await server.start();
  console.log(`Serving static files at http://${server.host}:${boundPort}`);
  process.once("SIGINT", () => void server.stop());
  process.once("SIGTERM", () => void server.stop());
}
