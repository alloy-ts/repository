# `gix` Crate API & `Repository` Binding Documentation

This document provides a comprehensive analysis of the Rust [`gix`](https://docs.rs/gix/latest/gix/) (gitoxide) crate API surface and how it maps to the JS/TS bindings exposed by `repository`.

---

## 1. Overview of `gix` (gitoxide)

`gix` is a pure-Rust, high-performance Git implementation. Its primary handle is the `Repository` struct, which acts as an entry point and hub into all Git operations (objects, references, remotes, revisions, worktree, status, configuration, etc.).

### Free Functions (Top-Level Constructors and Utilities)

The `gix` crate exports key top-level free functions for discovering, opening, initializing, and cloning repositories:

* `gix::open(path)` / `open_opts(...)` / `open_with_environment_overrides(...)`: Open an existing repository.
* `gix::init(path)` / `init_bare(path)`: Initialize a new repository with or without a working tree.
* `gix::discover(path)` / `discover_opts(...)`: Discover a Git repository by traversing upwards from `path`.
* `gix::prepare_clone(url, path)` / `prepare_clone_bare(url, path)`: Prepare a clone operation for fetching.
* `gix::config(...)` / `config_mut(...)` / `config_path(...)`: Access global/system Git configurations without an existing repository.

---

## 2. The `gix::Repository` Struct API

The `gix::Repository` struct is a single-threaded, thread-local handle that wraps references (`RefStore`) and objects (`OdbHandle`). Converting a `Repository` to `ThreadSafeRepository` via `.into_sync()` allows safe cross-thread sharing (`Send + Sync`).

### Key Functional Categories and Exported Methods

#### Objects & ODB Access
* `find_object(id)` / `try_find_object(id)`
* `find_commit(id)` / `find_tree(id)` / `find_blob(id)` / `find_tag(id)`
* `find_header(id)` / `try_find_header(id)` / `has_object(id)`
* `write_object(obj)` / `write_blob(data)` / `write_blob_stream(read)`
* `empty_tree()` / `empty_blob()`

#### References & HEAD
* `head()` / `head_id()` / `head_name()` / `head_ref()` / `head_commit()` / `head_tree()` / `head_tree_id()` / `head_tree_id_or_empty()`
* `find_reference(name)` / `try_find_reference(name)` / `references()`
* `reference(name, target, constraint, log_message)` / `edit_reference(...)` / `edit_references(...)` / `edit_references_as(...)`
* `tag(...)` / `tag_reference(...)`
* `namespace()` / `set_namespace(...)` / `clear_namespace()`

#### Commits, Revisions, & Graph Traversals
* `commit(ref, msg, tree, parents)` / `commit_as(...)` / `new_commit(...)` / `new_commit_as(...)`
* `rev_parse(spec)` / `rev_parse_single(spec)` / `rev_walk(tips)`
* `merge_base(a, b)` / `merge_base_with_graph(...)` / `merge_bases_many(...)` / `merge_base_octopus(...)`
* `commit_graph()` / `commit_graph_if_enabled()` / `revision_graph()`

#### Remotes & Fetching/Pushing
* `find_remote(name_or_url)` / `try_find_remote(...)` / `find_fetch_remote(...)` / `find_default_remote(...)`
* `remote_at(url)` / `remote_at_without_url_rewrite(...)`
* `remote_names()` / `remote_default_name(direction)`

#### Diff & Merge
* `diff_tree_to_tree(old, new, opts)` / `diff_resource_cache(...)` / `diff_resource_cache_for_tree_diff()` / `diff_algorithm()`
* `merge_trees(...)` / `merge_commits(...)` / `virtual_merge_base(...)`
* `blob_merge_options()` / `tree_merge_options()` / `merge_resource_cache(...)`

#### Worktree & Index Operations
* `worktree()` / `worktrees()` / `worktrees_including_main()` / `main_repo()`
* `add_worktree(...)` / `remove_worktree(...)` / `prepare_remove_worktree(...)` / `worktree_proxy_by_id(...)`
* `worktree_stream(...)` / `worktree_archive(...)`
* `index()` / `try_index()` / `index_or_empty()` / `index_or_load_from_head()` / `index_or_load_from_head_or_empty()` / `open_index()` / `index_from_tree()`

#### Repository Configuration, Paths & Status
* `config_snapshot()` / `config_snapshot_mut()` / `config_path(...)` / `config_file_mut(...)`
* `git_dir()` / `workdir()` / `set_workdir(...)` / `common_dir()` / `current_dir()` / `index_path()` / `modules_path()` / `prefix()`
* `kind()` / `object_hash()` / `is_bare()` / `is_pristine()` / `is_dirty()` / `is_shallow()`
* `shallow_commits()` / `shallow_file()` / `state()`
* `commit_signing_options()` / `commit_signing_options_if_enabled()`
* `editor()` / `editor_command()` / `filesystem_options()` / `stat_options()` / `open_options()` / `big_file_threshold()` / `ignore_pattern_parser()` / `ssh_connect_options()` / `command_context()`

#### Identity, Attributes, Submodules, Mailmap, Notes
* `author()` / `committer()` / `committer_or_set_fallback(...)` / `committer_or_set_generic_fallback()`
* `attributes(...)` / `attributes_only(...)` / `excludes(...)` / `filter_pipeline(...)`
* `open_mailmap()` / `open_mailmap_into(...)` / `open_modules_file()` / `modules()` / `submodules()` / `notes()`

---

## 3. The `repository` Node.js / TypeScript Binding Design Rationale

### Static Factory Constructors vs `new`
In Rust, `gix::open`, `gix::init`, and `gix::discover` are fallible free functions returning `Result<Repository, Error>`.
`napi-rs` constructors (`constructor` / `new`) cannot return custom JS error objects cleanly.
Therefore, `Repository` exposes static factory methods:
* `Repository.open(path: string): Repository`
* `Repository.init(path: string, bare?: boolean): Repository`
* `Repository.discover(path?: string): Repository`
* `Repository.clone(url: string, path: string, bare?: boolean): Promise<Repository>` (async worker thread)

### Binding Map Overview

| JS / TS Binding Method | Rust Implementation | Description |
| ---------------------- | ------------------- | ----------- |
| `Repository.open(path)` | `gix::ThreadSafeRepository::open` | Open repo at path |
| `Repository.init(path, bare)` | `gix::init` / `gix::init_bare` | Initialize new repo |
| `Repository.discover(path)` | `gix::discover` | Discover repo traversing parent dirs |
| `Repository.clone(url, path, bare)` | `gix::prepare_clone` | Asynchronously clone remote repo |
| `repo.head()` | `gix::Repository::head` | Return `Head` reference wrapper |
| `repo.headCommit()` | `gix::Repository::head_commit` | Return peeled `Commit` instance |
| `repo.headId()` | `gix::Repository::head_id` | Return SHA string of HEAD |
| `repo.findCommit(id)` | `gix::Repository::find_commit` | Find and decode commit object |
| `repo.findTree(id)` | `gix::Repository::find_tree` | Find and decode tree object |
| `repo.findBlob(id)` | `gix::Repository::find_blob` | Find blob (lazy content reading) |
| `repo.findTag(id)` | `gix::Repository::find_tag` | Find and decode tag object |
| `repo.findObject(id)` | `gix::Repository::find_object` | Return generic `GitObject` handle |
| `repo.log(rev, limit, firstParent)`| `gix::Repository::rev_walk` | Read commit history in one call |
| `repo.commit(ref, msg, tree, parents)`| `gix::Repository::commit` | Create a new commit and update ref |
| `repo.diffTreeToTree(old, new)` | `gix::Repository::diff_tree_to_tree` | Diff two tree SHAs |
| `repo.remotes()` / `findRemote(name)` | `gix::Repository::remotes` | Inspect and fetch remotes |
