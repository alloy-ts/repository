import { join } from "node:path";
import { beforeAll, describe, expect, test } from "vite-plus/test";
import { Repository } from "./main.ts";

// The error bindings guarantee, for every error crossing the FFI boundary:
//   1. `message` keeps the historical `"{context}: {root diagnostic}"` shape.
//   2. The gix error tree is attached as a standard JS `error.cause` chain —
//      one `Error` per node, in gix's logical traversal order, with the root
//      diagnostic first (it is also already the tail of `message`).
//   3. Classified nodes (gix_error `Class`) carry a machine-readable
//      `[git: <Class>]` suffix on their cause message.
//   4. Errors synthesized from plain messages (`err_str`) carry no cause chain.
let repo: Repository;

beforeAll(() => {
  // The bindings only need a repository to operate on; the project itself is one.
  repo = Repository.open(process.cwd());
});

/** Flatten a thrown error into its cause-chain messages (excluding `message` itself). */
function causeChain(error: unknown): string[] {
  const messages: string[] = [];
  let cause = (error as { cause?: { message: string; cause?: unknown } }).cause;
  while (cause) {
    messages.push(cause.message);
    cause = (cause as { cause?: { message: string } }).cause;
  }
  return messages;
}

/** Catch a throwing call and return the error (failing the test if nothing throws). */
function catchOf(throwing: () => unknown): Error {
  try {
    throwing();
  } catch (error) {
    return error as Error;
  }
  throw new Error("expected the call to throw, but it resolved");
}

describe("Repository error bindings", () => {
  test("messages keep the `{context}: {diagnostic}` shape", () => {
    const error = catchOf(() => repo.findCommit("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"));
    expect(error).toBeInstanceOf(Error);
    expect(error.message).toMatch(/^find_commit: /);
  });

  test("gix error trees are attached as a `cause` chain", () => {
    const error = catchOf(() => repo.findCommit("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"));
    const chain = causeChain(error);
    expect(chain.length, "at least one cause beyond the root diagnostic").toBeGreaterThan(0);
    expect(error.cause).toBeInstanceOf(Error);
  });

  test("not-found errors carry a `[git: NotFound]` classification tag", () => {
    const error = catchOf(() => repo.findCommit("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"));
    expect(causeChain(error)).toContainEqual(
      expect.stringContaining(
        "An object with id deadbeefdeadbeefdeadbeefdeadbeefdeadbeef could not be found [git: NotFound]",
      ),
    );
  });

  test("validation errors carry a `[git: Validation]` tag", () => {
    const error = catchOf(() => repo.findCommit("zzzz"));
    expect(error.message).toMatch(/^parse_oid: A hash sized 4 hexadecimal characters is invalid$/);
    expect(causeChain(error)[0]).toContain("[git: Validation]");
  });

  test("multi-cause trees are preserved in traversal order", () => {
    // Opening a non-repository produces a discovery error whose tree nests an
    // untagged wrapper between classified causes and the os-level root.
    const error = catchOf(() => Repository.open(join("/tmp", "definitely-not-a-repo-error-test")));
    const chain = causeChain(error);
    expect(chain.length).toBeGreaterThanOrEqual(3);
    expect(chain.some((message) => message.includes("[git: NotFound]"))).toBe(true);
    // The os error sits at the innermost end of the chain.
    expect(chain[chain.length - 1]).toMatch(/^No such file or directory/);
  });

  test("plain-message errors (`err_str`) carry no cause chain", () => {
    const error = catchOf(() =>
      repo.writeTag({
        name: "v0",
        target: "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
        targetKind: "bogus",
        message: "m",
      }),
    );
    expect(error.message).toBe("write_tag: invalid target_kind bogus");
    expect(error.cause).toBeUndefined();
  });
});
