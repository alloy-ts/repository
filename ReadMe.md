# repository

High-performance Node.js (Node-API / NAPI-RS) bindings for the Rust [`gitoxide`](https://github.com/GitoxideLabs/gitoxide) (`gix`) crate.

`repository` exposes `gix::Repository` capabilities to JavaScript and TypeScript, providing safe, high-speed Git operations with modern async support and full type definitions.

## Key Features

- **Repository Lifecycle**: `open()`, `openOpts()`, `init()`, `discover()`, and `clone()`.
- **References & Head**: Query and mutate HEAD, branches, tags, and custom references; atomic ref transactions via `editReference()`.
- **Objects & History**: Read and write blobs, trees, commits, annotated tags; rev-parse and rev-walk revision traversal.
- **Diff & Merge**: Tree-to-tree diffing, 3-way commit/tree merges, virtual merge bases, and file blaming.
- **Worktree & Index**: Status inspection, checkout, worktree proxy management, and streaming worktree archives.
- **Configuration & Trust**: Query and update global/repository git config, trust security mapping, and ref namespaces.
- **Filters & Attributes**: Content filter pipeline (`convert_to_git` / `convert_to_worktree`), pathspec matching, and `.gitattributes` inspection.
- **Submodules & Extensions**: Submodule configuration, mailmap identity resolution, and git notes.

## Installation & Build

Ensure you have Node.js and Rust installed on your machine.

```bash
# Install dependencies
npm install

# Build native NAPI bindings and TypeScript definitions
npm run build

# Run test suite
npm test
```

## Documentation

For full API reference, method signatures, domain organization, and comparison against Rust `gix::Repository`, see:

- [Repository API Documentation](docs/api/repository.md)

## License

[Unlicense](Licence)
