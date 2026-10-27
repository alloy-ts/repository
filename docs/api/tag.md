# Tag

Everything around tags: the `refs/tags/*` references (lightweight and
annotated), the annotated tag objects themselves, and the free functions for
validating tag names.

A git "tag" is two different things, and this API models both:

- A **tag reference** (`refs/tags/v1.0.0`) is a `Reference`. A _lightweight_ tag
  is nothing but this reference, pointing directly at any object.
- A **tag object** is a `Tag` — a small object carrying a name, message, tagger,
  and optional signature, pointing at another object. An _annotated_ tag is a
  tag reference pointing at a tag object.

```ts
repo.createTag("v1.0.0", "HEAD"); // lightweight: ref -> commit
repo.createTag("v1.1.0", "HEAD", { message: "release\n" }); // annotated: ref -> tag object -> commit
```

## `Repository` — discovery

| Method             | Signature                       | Notes                                                                                                  |
| ------------------ | ------------------------------- | ------------------------------------------------------------------------------------------------------ |
| `tags`             | `() => Reference[]`             | All `refs/tags/*` references (lightweight and annotated), like `localBranches()`.                      |
| `tagNames`         | `() => string[]`                | Short names (`v1.0.0`), sorted. Cheaper than `tags()` — references are not materialised.               |
| `findTag`          | `(id: string) => Tag`           | Tag object by 40-hex object id. Throws when missing or not a tag object.                               |
| `findTagByName`    | `(name: string) => Tag`         | Short (`v1.0.0`) or full (`refs/tags/v1.0.0`) name. Throws when the tag is missing **or lightweight**. |
| `tryFindTagByName` | `(name: string) => Tag \| null` | Like `findTagByName`, but `null` when the tag does not exist. Still throws for lightweight tags.       |

`findTagByName`/`tryFindTagByName` return the tag _object_, so they only work
for annotated tags. For lightweight tags use `findReference("refs/tags/<name>")`
— the reference's `target` is the tagged object id — or `ref.peelToId()` to peel
through to the final object.

## `Repository` — creation

| Method                 | Signature                                                | Notes                                                                           |
| ---------------------- | -------------------------------------------------------- | ------------------------------------------------------------------------------- |
| `createTag`            | `(name, target, messageOrOptions?, force?) => Reference` | Lightweight by default; annotated with a message; signed with `sign: true`.     |
| `createLightweightTag` | `(name, target, force?) => Reference`                    | Alias of `tagReference`.                                                        |
| `tagReference`         | `(name, target, force?) => Reference`                    | Create/overwrite a `refs/tags/<name>` reference pointing directly at `target`.  |
| `writeTag`             | `(input: TagInput) => string`                            | Write an annotated tag _object_ only — no reference is created. Returns its id. |

All of these accept `name` as a short (`v1.0.0`) or fully qualified
(`refs/tags/v1.0.0`) tag name; a name that already carries the `refs/tags/`
prefix is used as-is, never prefixed twice. `target` is a 40-hex object id (for
`createTag` it may also be a revision such as `HEAD`). Without `force`, an
existing tag of the same name is an error.

`createTag`'s third argument is either the legacy message string —
`createTag(name, target, "message", force?)` — or an options object; omit it for
a lightweight tag:

```ts
{
  message?: string;    // present (or annotated/sign) => annotated tag object
  annotated?: boolean; // force an annotated tag object with an empty message
  tagger?: Signature;  // { name, email, time, timezoneOffset }; default: configured committer
  sign?: boolean;      // `git tag -s` — sign with user.signingKey / gpg.format; implies annotated
  targetKind?: string; // "commit" | "tree" | "blob" | "tag"; inferred from the object when omitted
  force?: boolean;     // overwrite an existing tag
}
```

`TagInput` (`writeTag`) mirrors the annotated case:
`{ name, target, targetKind?,
message, tagger?, signature?, sign? }` —
`signature` embeds a pre-computed armored signature, `sign` computes one via the
configured signing program.

**Signing produces a new object.** `git tag -s` semantics are
`createTag(name,
target, { message, sign: true })`, which writes the signed
object _and_ moves the reference. In contrast, `Tag.sign()` and
`writeTag({ sign: true })` only write new objects; any existing
`refs/tags/<name>` keeps pointing at the unsigned one.

## `Repository` — mutation

| Method       | Signature                                 | Notes                                                                  |
| ------------ | ----------------------------------------- | ---------------------------------------------------------------------- |
| `deleteTags` | `(names: string[]) => string[]`           | Delete tags (short or full names); returns the full ref names deleted. |
| `renameTag`  | `(oldName, newName, force?) => Reference` | Move the reference; throws when the destination exists unless `force`. |

`renameTag` only renames the _reference_: an annotated tag object keeps the name
it was created with, so `tag.name` still reads the old name afterwards. Recreate
the tag if the embedded name matters.

## `Tag`

A decoded annotated tag object:

| Member         | Signature                     | Notes                                                                             |
| -------------- | ----------------------------- | --------------------------------------------------------------------------------- |
| `id`           | `string` (getter)             | Object id of the tag object itself.                                               |
| `name`         | `string` (getter)             | The tag name embedded in the object (`v1.0.0`).                                   |
| `message`      | `string` (getter)             | Full tag message.                                                                 |
| `targetId`     | `string` (getter)             | Object id the tag points at.                                                      |
| `targetKind`   | `string` (getter)             | `commit`, `tree`, `blob`, or `tag`.                                               |
| `signature`    | `string \| null` (getter)     | Armored in-body signature, if present. Presence ≠ validity — see `verify()`.      |
| `target`       | `() => GitObject`             | The object the tag points at.                                                     |
| `peelToCommit` | `() => Commit`                | Follow tag chains to a commit; throws if unreachable.                             |
| `peelToTree`   | `() => Tree`                  | Follow tag chains to a tree.                                                      |
| `peelToBlob`   | `() => Blob`                  | Follow tag chains to a blob.                                                      |
| `peelToKind`   | `(kind) => GitObject`         | Peel to `commit`/`tree`/`blob`/`tag`.                                             |
| `data`         | `() => Buffer`                | Raw, decompressed tag object body.                                                |
| `tagger`       | `() => Signature \| null`     | The tagger identity (`null` when the object has no tagger field).                 |
| `sign`         | `() => Tag`                   | Signed copy as a _new_ object; the original and any reference are untouched.      |
| `verify`       | `() => VerifyOutcome \| null` | `null` when unsigned; otherwise the verification outcome (shells out to GPG/SSH). |

## Free functions

| Function         | Signature                   | Notes                                                                            |
| ---------------- | --------------------------- | -------------------------------------------------------------------------------- |
| `tagNameIsValid` | `(name: string) => boolean` | Valid short (`v1.0.0`) or `refs/tags/…` tag name, mirroring `branchNameIsValid`. |

## End-to-end example

```ts
import { Repository, tagNameIsValid } from "./src/main.ts";

const repo = Repository.open(".");

// Create a signed release tag on HEAD.
if (!tagNameIsValid("v2.0.0")) throw new Error("bad tag name");
const ref = repo.createTag("v2.0.0", "HEAD", {
  message: "release v2.0.0\n",
  sign: true,
});

// Inspect it.
const tag = repo.findTagByName("v2.0.0");
console.log(tag.name, tag.tagger()?.email, tag.targetKind);
const commit = tag.peelToCommit(); // the tagged commit, no repo round-trip
const outcome = tag.verify(); // VerifyOutcome | null

// List and manage.
for (const name of repo.tagNames()) console.log(name);
repo.renameTag("v2.0.0-rc", "v2.0.0", /* force */ true);
repo.deleteTags(["v1.0.0", "v1.1.0"]);
```

## Error behavior

- Lookups throw when the tag (or its tag object) is missing; `tryFindTagByName`
  returns `null` instead — but still throws for lightweight tags, which have no
  tag object.
- `deleteTags` requires each tag to exist (`must_exist` semantics) and fails the
  whole batch otherwise.
- `createTag`/`tagReference` throw when the tag exists unless `force` is set;
  `renameTag` throws when the destination exists unless `force` is set.
- Invalid target kinds (`targetKind: "bogus"`) are rejected with
  `invalid target_kind bogus`.
