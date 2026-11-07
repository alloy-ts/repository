# Trailer API

Parsing, formatting, and manipulating Git commit message trailers (`Signed-off-by:`, `Co-authored-by:`, etc.) following RFC 822 / Git trailer conventions.

## Overview

The `trailer` module (`sources/repo/src/trailer.rs`) provides the `Trailer` class and standalone helpers to parse, extract, and format trailers from commit message descriptions.

## Functions

### `message_trailers(message: string): Trailer[]`

Parses all trailers located in the trailer block (the final paragraph) of a commit message.

### `format_trailer(key: string, value: string): string`

Formats a single trailer key-value pair as `Key: Value\n`.

### `format_trailers(trailers: Trailer[]): string`

Formats an array of trailers into a newline-separated trailer block string.

---

## `Trailer` Class

Represents a single parsed Git commit trailer key-value pair.

### Constructor

```ts
new Trailer(key: string, value: string)
```

### Properties

| Property | Type     | Description                                                |
| -------- | -------- | ---------------------------------------------------------- |
| `key`    | `string` | The trailer token / key (e.g., `"Signed-off-by"`).         |
| `token`  | `string` | Alias for `key`.                                           |
| `value`  | `string` | The trailer value (e.g., `"Jane Doe <jane@example.com>"`). |

### Methods

- `format(): string`: Formats the trailer as `"key: value"`.
- `static parseTrailers(message: string): Trailer[]`: Static alias for parsing trailers from a commit message.
- `static parseDescriptionTrailers(description: string): Trailer[]`: Parses trailers specifically from a commit description.

## Example

```ts
import { Trailer, message_trailers, format_trailers } from "@alloy-ts/repository";

const commitMsg = `feat: add awesome feature\n\nSigned-off-by: Alice <alice@example.com>\nCo-authored-by: Bob <bob@example.com>`;

const trailers = message_trailers(commitMsg);
console.log(trailers.length); // 2
console.log(trailers[0].key); // "Signed-off-by"
console.log(trailers[0].value); // "Alice <alice@example.com>"

const formatted = format_trailers(trailers);
console.log(formatted);
```
