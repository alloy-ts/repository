# Commit

Everything around commit objects: reading a commit's identity, message parts,
headers, tree, and parents, resolving identities through the mailmap, verifying
and creating signatures, amending, and a pair of standalone message helpers.

Commit objects are addressed by their 40-character (SHA-1) hex id, like
everywhere else. A `Commit` is a lightweight handle: it carries only the id and
a reference to the repository, so every accessor re-reads and re-decodes the
object — safe to hold onto across mutations, but not a cache.

## `Commit`

### Identity and message

| Method       | Signature              | Notes                                                                                                     |
| ------------ | ---------------------- | --------------------------------------------------------------------------------------------------------- |
| `id`         | `() => string`         | Hex id of the commit.                                                                                     |
| `message`    | `() => string`         | The message with leading newlines removed (libgit2's `git_commit_message`).                               |
| `messageRaw` | `() => string`         | The message exactly as stored — the two differ only for messages that start with blank lines.             |
| `summary`    | `() => string \| null` | The first paragraph with whitespace folded (git's rules); `null` when the message has no subject.         |
| `body`       | `() => string \| null` | Everything after the first paragraph, trimmed — **trailers included** (libgit2 parity); `null` when none. |
| `author`     | `() => Signature`      | `{ name, email, time, timezoneOffset }`.                                                                  |
| `committer`  | `() => Signature`      | Same shape as `author`.                                                                                   |
| `encoding`   | `() => string \| null` | The `encoding` header; `null` means UTF-8 should be assumed.                                              |

`time`, `timezoneOffset` and `CommitTime.offset` are all in **seconds**. Note
that libgit2 reports `git_commit_time_offset` in minutes — this binding does
not.

### Headers

| Method         | Signature                          | Notes                                                                                                                                                                                                                                   |
| -------------- | ---------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `rawHeader`    | `() => string`                     | The raw header block, up to (and including the newline before) the blank line (`git_commit_raw_header`).                                                                                                                                |
| `headerField`  | `(name: string) => string \| null` | Unfolded value of any header — `tree`, `parent`, `author`, `committer`, `encoding`, `gpgsig`, or a custom one. Repeated headers (e.g. `parent`) are joined with newlines, like libgit2's `git_commit_header_field`. `null` when absent. |
| `extraHeaders` | `() => CommitHeaderEntry[]`        | Every header outside git's standard set, in document order: `{ name, value }`.                                                                                                                                                          |

Commit headers are `key<space>value` (not `key: value`); `headerField` and
`rawHeader` both work on the raw object bytes, so custom and multi-line headers
(`gpgsig`, `mergetag`) are preserved verbatim.

### Time, tree, and parents

| Method        | Signature                   | Notes                                                                |
| ------------- | --------------------------- | -------------------------------------------------------------------- |
| `time`        | `() => CommitTime`          | `{ seconds, offset }` — the committer time.                          |
| `tree`        | `() => Tree`                | The root tree, materialised (libgit2's `git_commit_tree`).           |
| `treeId`      | `() => string`              | The root tree id — no object database access, cheaper than `tree()`. |
| `parentCount` | `() => number`              | Number of parents (`0` for a root commit).                           |
| `parentIds`   | `() => string[]`            | Parent ids in order, without loading the parent objects.             |
| `parents`     | `() => Commit[]`            | All parents, materialised and in order.                              |
| `parent`      | `(index: number) => Commit` | A single parent; throws when `index` is out of bounds.               |

### Mailmap

| Method                 | Signature                         | Notes                                                                                |
| ---------------------- | --------------------------------- | ------------------------------------------------------------------------------------ |
| `authorWithMailmap`    | `(mailmap: Mailmap) => Signature` | The author as canonicalised by the mailmap; the original identity when nothing maps. |
| `committerWithMailmap` | `(mailmap: Mailmap) => Signature` | Same for the committer.                                                              |

Both preserve the original timestamp — mailmap entries only rewrite the name and
email. Obtain a mailmap with `repo.openMailmap()` (it returns `null` when the
repository has no `.mailmap` and nothing configured).

```ts
const mailmap = repo.openMailmap();
if (mailmap) {
  const author = commit.authorWithMailmap(mailmap);
}
```

### Signatures

| Method      | Signature                     | Notes                                                                                                                                                                                                                        |
| ----------- | ----------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `signature` | `() => string \| null`        | The raw `gpgsig`/`gpgsig-sha256` header. Presence does **not** mean the signature is valid.                                                                                                                                  |
| `verify`    | `() => VerifyOutcome \| null` | `null` when unsigned. Otherwise `{ format, status, trustLevel, signer, key, fingerprint, primaryKeyFingerprint, output, rawOutput, valid }`. Shells out to the configured GPG/GPGSM/`ssh-keygen` program, so it can be slow. |
| `sign`      | `() => Commit`                | A **new** commit object carrying a signature, using `user.signingKey`/`gpg.format` like `git commit -S`. The original commit is untouched and no reference moves.                                                            |

### Objects

| Method     | Signature      | Notes                                                           |
| ---------- | -------------- | --------------------------------------------------------------- |
| `asObject` | `() => Object` | The same object, generically typed (`.data()`, `.asTree()`, …). |

### Amending

| Method  | Signature                           | Notes                                                                                                   |
| ------- | ----------------------------------- | ------------------------------------------------------------------------------------------------------- |
| `amend` | `(options: AmendOptions) => string` | Creates a new commit from this one with the given fields replaced, and returns the new commit's **id**. |

`AmendOptions` — every field is optional and carries over from the original
commit when omitted:

```ts
{
  updateRef?: string;        // ref to move to the new commit, e.g. "HEAD"
  author?: Signature;        // name, email *and* time
  committer?: Signature;     // name, email *and* time
  messageEncoding?: string;
  message?: string;
  tree?: string;             // replacement root tree id
}
```

`amend` never moves a reference unless `updateRef` is set. A symbolic `HEAD` is
dereferenced, so `amend({ updateRef: "HEAD" })` rewrites the branch HEAD points
at rather than detaching it — the same behavior as `git commit --amend`.

```ts
const base = repo.headCommit();
const id = repo.writeCommit({
  tree: base.treeId,
  parents: [base.id],
  message: "wip",
});
const amended = repo.findCommit(id).amend({
  message: "feat: done\n\nlonger body",
});
repo.findCommit(amended).summary; // "feat: done"
```

### Trailers

| Method      | Signature         | Notes                                                                                  |
| ----------- | ----------------- | -------------------------------------------------------------------------------------- |
| `trailers`  | `() => Trailer[]` | All `git interpret-trailers` trailers from the message's trailing paragraph, in order. |
| `coAuthors` | `() => Trailer[]` | Just the `Co-authored-by` trailers — the convenient case.                              |

`Trailer` is `{ token: string, value: string }` with the value trimmed and
multi-line values unfolded.

```ts
commit.trailers(); // [{ token: "Signed-off-by", value: "Jane <jane@example.com>" }]
commit.coAuthors(); // [{ token: "Co-authored-by", value: "A U Thor <a@example.com>" }]
```

## Creating commits (`Repository`)

| Method         | Signature                                                                                | Notes                                                                                                            |
| -------------- | ---------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------- |
| `findCommit`   | `(id: string) => Commit`                                                                 | Throws when the object is missing or is not a commit.                                                            |
| `headCommit`   | `() => Commit`                                                                           | The commit `HEAD` currently points to — an alias of `headCommit()`. Read `commit.id` for the sha.                |
| `headCommitId` | `() => string \| null`                                                                   | Hex sha of the commit `HEAD` points to, or `null` when `HEAD` is unborn. Convenience for build banners/versions. |
| `writeCommit`  | `(input: CommitInput) => string`                                                         | Writes a loose commit object without touching any reference. Returns the id.                                     |
| `newCommit`    | `(message: string, tree: string, parents: string[]) => string`                           | Like `writeCommit`, using the configured author/committer identities. No ref is moved.                           |
| `newCommitAs`  | `(committer: Signature, author: Signature, message, tree, parents) => string`            | Like `newCommit` with explicit identities, which also fixes both timestamps.                                     |
| `commit`       | `(reference: string, message: string, tree: string, parents: string[]) => string`        | Creates a commit **and** moves `reference` to it.                                                                |
| `commitAs`     | `(committer: Signature, author: Signature, reference, message, tree, parents) => string` | Like `commit` with explicit identities.                                                                          |

`CommitInput` is `{ tree, parents, message, author?, committer? }` — the
identities default to the repository configuration and throw when none is
configured. Passing explicit `Signature`s fixes the author and commit times.

None of these run hooks, sign by default, or touch the worktree/index — they are
object- and reference-level operations only.

## Message helpers

Two free functions (no repository needed) complement the commit API:

| Function          | Signature                                           | Notes                                                                                                                                                                                                                                                                                                                                        |
| ----------------- | --------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `messagePrettify` | `(message: string, commentChar?: string) => string` | git's message cleanup: trims trailing whitespace per line, collapses blank-line runs to one, strips leading/trailing blank lines, and appends exactly one newline. With `commentChar` (git's default is `"#"`), lines starting with it are dropped. Must be a single character; an empty result stays empty. Mirrors `git_message_prettify`. |
| `messageTrailers` | `(message: string) => Trailer[]`                    | The trailers of an arbitrary message, not one attached to a commit. Everything after a `---` cut line is ignored, like git. Mirrors `git_message_trailers_strs`.                                                                                                                                                                             |

libgit2's `git_message_trailers_bytes` has no binding: JS strings are always
valid UTF-8, so the bytes variant has nothing to operate on.

```ts
messagePrettify("1\n\n\n2"); // "1\n\n2\n"
messagePrettify("1\n# note\n# more", "#"); // "1\n"

messageTrailers("subject\n\nSigned-off-by: A <a@e>\n---\nnoise");
// [{ token: "Signed-off-by", value: "A <a@e>" }]
```

## Divergences from libgit2

- `time.offset` (and `Signature.timezoneOffset`) are in **seconds**, not
  minutes.
- `message` strips leading newlines like libgit2's `git_commit_message`; the
  verbatim message is `messageRaw`.
- There is no `parents()` iterator — `parents()` returns an array, and
  `parent(index)` fetches a single parent.
- `amend` returns the new commit id (as libgit2 does), and takes a single
  options object instead of six positional optionals.
- `git_message_trailers_bytes` is intentionally unbound (see above).
