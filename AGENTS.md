# Agent instructions

## Development environment

This project builds a Rust `cdylib` exposed through N-API. Use a Rust toolchain
supporting edition 2024, Node.js 24, and npm 11.11.0 (as required by
`package.json`). Install Rust with Cargo available on `PATH`.

On Debian 13, configure the reusable development environment with these system
packages: `build-essential`, `pkg-config`, `cmake`, `clang-19`,
`libclang-19-dev`, `libclang-common-19-dev`, and `libsqlite3-dev`. Matching
Clang libraries and resource headers are needed by `bindgen` when compiling
`libuv-sys2`. SQLite development files are required when linking Diesel.
A workspace-local SQLite installation plus the GCC-header workaround below
produced a successful `cargo build`. The proposed system-package setup has
not yet been validated in a freshly provisioned environment.

Put provisioning commands in the cloud environment setup or image configuration.
This file documents requirements; it does not install packages automatically.
Keep system package installation out of npm lifecycle scripts. After updating
the reusable environment, rebuild it and validate from a clean checkout.

Run from the repository root:

```bash
npm install
cargo build
npm run build
npm run test
```

Do not use bare `cargo install`: this crate has no installable binary.
`cargo build` validates the Rust library; `npm run build` must also generate
the JavaScript bindings and package artifacts before TypeScript tests can run.

If `bindgen` reports `stddef.h` missing, first check the matching Clang resource
headers. A temporary workaround using existing GCC headers is:

```bash
BINDGEN_EXTRA_CLANG_ARGS="-isystem $(gcc -print-file-name=include)" cargo build
```

## Verified environment recovery

On 2026-11-07, `cargo build` at commit `50ff094` succeeded with 64 warnings
and no errors after both of these environment fixes:

- Point `bindgen` at GCC's existing standard headers to resolve `stddef.h`.
- Provide SQLite development libraries and add their directory to
  `LIBRARY_PATH` so the C linker can resolve `-lsqlite3`.

The environment had no `sudo` or scoped package installer, and
`apt-get download libsqlite3-dev` failed because package metadata was
unavailable. The successful fallback downloaded Debian 13 amd64 packages
`libsqlite3-dev` and `libsqlite3-0`, both version `3.46.1-7+deb13u2`, from
`https://deb.debian.org/debian/pool/main/s/sqlite3/` and extracted them with
`dpkg-deb -x` into a workspace-owned directory. Use packages matching the
current distribution and architecture; do not reuse these versions blindly.

For the existing extracted packages in this session, the successful command
was:

```bash
export PATH=/workspace/work/cargo/bin:$PATH
export CARGO_HOME=/workspace/work/cargo
export RUSTUP_HOME=/workspace/work/rustup
export BINDGEN_EXTRA_CLANG_ARGS="-isystem $(gcc -print-file-name=include)"
export SQLITE3_LIB_DIR=/workspace/work/system-packages/root/usr/lib/x86_64-linux-gnu
export SQLITE3_INCLUDE_DIR=/workspace/work/system-packages/root/usr/include
export LIBRARY_PATH=/workspace/work/system-packages/root/usr/lib/x86_64-linux-gnu${LIBRARY_PATH:+:$LIBRARY_PATH}
cargo build
```

These paths are session-specific. The extracted files must exist before using
them. Setting `SQLITE3_LIB_DIR` and `SQLITE3_INCLUDE_DIR` alone did not resolve
the linker failure in this environment; adding `LIBRARY_PATH` did.
The system image was not modified, and these exports do not persist into a
new shell. Prefer installing matching Clang packages and `libsqlite3-dev` in
the reusable environment setup, then validate without the local workaround.

This verifies only the Rust library build. It does not verify generated
JavaScript bindings, the npm build, or test assertions.

## Native build sequencing

`cli.build(...)` returns an object whose `task` promise completes compilation
and binding generation. Await that promise before importing `dist/index.js`,
returning in silent mode, or starting packaging. Propagate build failures.

The baseline at commit `50ff094` failed to collect all 76 test files because
generated modules were missing; no test assertions executed in that run.

## N-API reference notes

Unlike defining modules in Node.js, we don't need to explicitly register exports
like `module.exports.xxx = xxx`.

The `#[napi]` macro will automatically generate module registering code for you.
This auto registering idea was inspired by
[node-bindgen](https://github.com/infinyon/node-bindgen).

:::

## `Function`

Exporting a function is incredibly simple. Just decorate a normal rust function
with `#[napi]`:

**lib.rs**

```rust
#[napi]
pub fn sum(a: f64, b: f64) -> f64 {
	a + b
}
```

## `Const`

**lib.rs**

```rust
#[napi]
pub const DEFAULT_COST: u32 = 12;
```

**index.d.ts**

```ts
export const DEFAULT_COST: number;
```

## `Class`

See [`class section`](./class) for more details.

**lib.rs**

```rust
#[napi(constructor)]
pub struct Animal {
  pub name: String,
  pub kind: u32,
}

#[napi]
impl Animal {
  #[napi]
  pub fn change_name(&mut self, new_name: String) {
    self.name = new_name;
  }
}
```

## `Enum`

See [`enum section`](./enum) for more details.

**lib.rs**

```rust
#[napi]
pub enum Kind {
  Dog,
  Cat,
  Duck,
}
```

## `exports` object

You can use the `#[napi(module_exports)]` attribute to access the `exports`
object.

**lib.rs**

```rust
use napi::bindgen_prelude::*;
use napi_derive::napi;

#[napi(module_exports)]
pub fn exports(mut export: Object) -> Result<()> {
  let symbol = Symbol::new("NAPI_RS_SYMBOL");
  export.set_named_property("NAPI_RS_SYMBOL", symbol)?;
  Ok(())
}
```

## Namespaces

For larger addons you can group related exports into nested JavaScript module
objects instead of flattening everything onto the root `exports`. There are two
ways:

- `#[napi] mod name { ... }` — export an inline Rust module as a namespace.
  Every child item that also carries `#[napi]` is exported inside it (nested
  napi modules are not supported). Add `#[napi(js_name = "...")]` on the `mod`
  to rename the namespace object.
- `#[napi(namespace = "...")]` on individual functions, classes, impl blocks,
  enums, consts, and type aliases — registers that item under
  `exports.<namespace>`; apply the same namespace to a class and its `impl`
  blocks. See
  [`namespace` in the attributes reference](/docs/concepts/napi-attributes#naming-and-exports).

The canonical example is
[`examples/napi/src/js_mod.rs`](https://github.com/napi-rs/napi-rs/blob/main/examples/napi/src/js_mod.rs):

**lib.rs**

```rust
#[napi]
mod xxh3 {
  use napi::bindgen_prelude::{BigInt, Buffer};

  #[napi]
  pub const ALIGNMENT: u32 = 16;

  #[napi(js_name = "xxh3_64")]
  pub fn xxh64(input: Buffer) -> u64 {
    let mut h: u64 = 0;
    for i in input.as_ref() {
      h = h.wrapping_add(*i as u64);
    }
    h
  }

  #[napi]
  pub struct Xxh3 {
    inner: BigInt,
  }

  #[napi]
  impl Xxh3 {
    #[napi(constructor)]
    pub fn new() -> Xxh3 {
      // ...
    }
  }
}

#[napi]
mod xxh2 {
  use napi::bindgen_prelude::*;

  #[napi]
  pub fn xxh2_plus(a: u32, b: u32) -> u32 {
    a + b
  }
}
```

The members are reached through the namespace objects on the package exports:

**index.ts**

```ts
import { xxh2, xxh3 } from "./index.js";

xxh3.xxh3_64(Buffer.from("hello")); // function renamed with js_name
console.log(xxh3.ALIGNMENT); // 16
const hasher = new xxh3.Xxh3(); // classes live inside the namespace too
xxh2.xxh2Plus(1, 2); // 3
```

And the generated `.d.ts` mirrors the nesting with `export declare namespace`:

**index.d.ts**

```ts
export declare namespace xxh2 {
  export function xxh2Plus(a: number, b: number): number;
}

export declare namespace xxh3 {
  export class Xxh3 {
    constructor();
  }
  export const ALIGNMENT: number;
  export function xxh3_64(input: Buffer): bigint;
}
```
