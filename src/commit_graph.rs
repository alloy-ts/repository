use std::convert::Infallible;
use std::sync::Arc;

use napi::Result;
use napi_derive::napi;

use crate::error::{err, err_str, parse_oid, thread_local};
use crate::repository::Repository;

/// Information about the commit-graph, if present.
#[napi(object)]
pub struct CommitGraphInfo {
    /// Number of commits recorded in the commit-graph.
    pub commit_count: f64,
}

/// A single `(parents -> count)` entry from a commit-graph verification.
#[napi(object)]
pub struct CommitParentCount {
    /// Number of parents the commit has.
    pub parents: f64,
    /// Number of commits in the graph that have exactly that many parents.
    pub count: f64,
}

/// The result of verifying a commit-graph's integrity.
#[napi(object)]
pub struct CommitGraphOutcome {
    /// The length of the longest path between any two commits in this graph.
    ///
    /// `null` when the longest path is too large to fit into a `u32`.
    pub longest_path_length: Option<f64>,
    /// The total number of commits traversed.
    pub num_commits: f64,
    /// A mapping of `N -> number of commits with N parents`.
    pub parent_counts: Vec<CommitParentCount>,
}

/// A single commit as stored in the commit-graph, with metadata resolved to owned values.
#[napi(object)]
pub struct GraphCommit {
    /// The commit's object id (hex).
    pub id: String,
    /// The root tree id the commit points to (hex).
    pub root_tree_id: String,
    /// The commit's generation number (`0` means uncomputed / legacy).
    pub generation: f64,
    /// Positions of this commit's parents within the graph.
    pub parent_positions: Vec<f64>,
    /// Object ids (hex) of this commit's parents, resolved from `parent_positions`.
    pub parent_ids: Vec<String>,
}

/// Convert a borrowed `gix` commit (tied to `g`) into an owned, FFI-safe [`GraphCommit`].
fn to_graph_commit(
    g: &gix::commitgraph::Graph,
    commit: gix::commitgraph::file::Commit<'_>,
) -> Result<GraphCommit> {
    let id = commit.id().to_string();
    let root_tree_id = commit.root_tree_id().to_string();
    let generation = commit.generation() as f64;
    let mut parent_positions = Vec::new();
    let mut parent_ids = Vec::new();
    // `iter_parents` consumes `commit`, so `id`/`root_tree_id`/`generation` must be read first.
    for parent in commit.iter_parents() {
        let p = parent.map_err(|e| err("commit.parents", e))?;
        parent_positions.push(p.0 as f64);
        parent_ids.push(g.id_at(p).to_string());
    }
    Ok(GraphCommit {
        id,
        root_tree_id,
        generation,
        parent_positions,
        parent_ids,
    })
}

fn parse_hash_kind(s: &str) -> Result<gix::hash::Kind> {
    match s {
        "Sha1" => Ok(gix::hash::Kind::Sha1),
        "Sha256" => Ok(gix::hash::Kind::Sha256),
        other => Err(err_str(
            "hash_kind",
            &format!("unknown object hash kind: {other} (expected \"Sha1\" or \"Sha256\")"),
        )),
    }
}

/// A complete commit-graph, wrapping `gix::commitgraph::Graph`.
///
/// The data may come from a monolithic `objects/info/commit-graph` file, or from one or more
/// `objects/info/commit-graphs/graph-*.graph` files. Obtain one via
/// [`Repository::graph`] / [`Repository::graph_if_enabled`].
#[napi]
pub struct Graph {
    inner: Arc<gix::commitgraph::Graph>,
}

#[napi]
impl Graph {
    /// The number of commits stored in this graph.
    #[napi(getter)]
    pub fn num_commits(&self) -> f64 {
        self.inner.num_commits() as f64
    }

    /// The kind of hash used in this graph (for example `Sha1` or `Sha256`).
    #[napi(getter)]
    pub fn object_hash(&self) -> String {
        format!("{:?}", self.inner.object_hash())
    }

    /// Translate the given commit id to its position in the graph, or `null` if it is absent.
    #[napi]
    pub fn lookup(&self, id: String) -> Result<Option<f64>> {
        let oid = parse_oid(&id)?;
        Ok(self.inner.lookup(oid).map(|p| p.0 as f64))
    }

    /// Return the commit id stored at the given position.
    ///
    /// # Errors
    /// If `pos` is not a non-negative integer within the `u32` range, or if it is
    /// greater than or equal to [`num_commits`](Self::num_commits).
    #[napi]
    pub fn id_at(&self, pos: f64) -> Result<String> {
        if pos < 0.0 || pos.fract() != 0.0 || pos > u32::MAX as f64 {
            return Err(err_str(
                "id_at",
                "position must be a non-negative integer within u32 range",
            ));
        }
        let pos = pos as u32;
        if pos >= self.inner.num_commits() {
            return Err(err_str("id_at", "position is out of range"));
        }
        Ok(self
            .inner
            .id_at(gix::commitgraph::Position(pos))
            .to_string())
    }

    /// Verify the integrity of the commit-graph (checksums and generation numbers) and return a
    /// summary of the traversal.
    #[napi]
    pub fn verify_integrity(&self) -> Result<CommitGraphOutcome> {
        let outcome = self
            .inner
            .verify_integrity(|_commit| Ok::<_, Infallible>(()))
            .map_err(|e| err("verify_integrity", e))?;
        Ok(CommitGraphOutcome {
            longest_path_length: outcome.longest_path_length.map(|v| v as f64),
            num_commits: outcome.num_commits as f64,
            parent_counts: outcome
                .parent_counts
                .into_iter()
                .map(|(parents, count)| CommitParentCount {
                    parents: parents as f64,
                    count: count as f64,
                })
                .collect(),
        })
    }

    /// Return the commit stored at `position`, or `null` if `position` is out of range.
    ///
    /// Note: gix panics on out-of-range access, so this guards the range and returns `null` instead.
    #[napi]
    pub fn commit_at(&self, position: f64) -> Result<Option<GraphCommit>> {
        if position < 0.0 || position.fract() != 0.0 || position > u32::MAX as f64 {
            return Err(err_str(
                "commit_at",
                "position must be a non-negative integer within u32 range",
            ));
        }
        let pos = position as u32;
        if pos >= self.inner.num_commits() {
            return Ok(None);
        }
        Ok(Some(to_graph_commit(
            &self.inner,
            self.inner.commit_at(gix::commitgraph::Position(pos)),
        )?))
    }

    /// Return the commit matching `id`, or `null` if it is not present in the graph.
    #[napi]
    pub fn commit_by_id(&self, id: String) -> Result<Option<GraphCommit>> {
        let oid = parse_oid(&id)?;
        match self.inner.commit_by_id(oid) {
            Some(commit) => Ok(Some(to_graph_commit(&self.inner, commit)?)),
            None => Ok(None),
        }
    }

    /// All commit ids in the graph (unsorted), as hex strings.
    #[napi]
    pub fn iter_ids(&self) -> Vec<String> {
        self.inner.iter_ids().map(|oid| oid.to_string()).collect()
    }

    /// All commits in the graph (unsorted), as owned [`GraphCommit`] values.
    #[napi]
    pub fn iter_commits(&self) -> Result<Vec<GraphCommit>> {
        let mut out = Vec::new();
        for commit in self.inner.iter_commits() {
            out.push(to_graph_commit(&self.inner, commit)?);
        }
        Ok(out)
    }

    /// Open a commit-graph from `path` (a single graph file or a directory containing graph files).
    ///
    /// The hash kind is read from the commit-graph file itself. `object_hash` is accepted for
    /// backward compatibility and must be `"Sha1"` or `"Sha256"` (it is validated but no longer
    /// selects the parser). `alloc_limit_bytes` is accepted for compatibility and currently unused.
    #[napi]
    pub fn at(
        path: String,
        object_hash: String,
        alloc_limit_bytes: Option<f64>,
    ) -> Result<Graph> {
        // Validate the caller's hash-kind hint (the parser auto-detects from the file).
        let _kind = parse_hash_kind(&object_hash)?;
        let _ = alloc_limit_bytes;
        let g = gix::commitgraph::Graph::at(std::path::Path::new(&path))
            .map_err(|e| err("commit_graph.at", e))?;
        Ok(Graph {
            inner: Arc::new(g),
        })
    }
}

#[napi]
impl Repository {
    /// Return info about the commit-graph if it is present *and* enabled, else `null`.
    #[napi]
    pub fn commit_graph_if_enabled(&self) -> Result<Option<CommitGraphInfo>> {
        let r = thread_local(&self.inner);
        match r
            .commit_graph_if_enabled()
            .map_err(|e| err("commit_graph_if_enabled", e))?
        {
            Some(graph) => Ok(Some(CommitGraphInfo {
                commit_count: graph.num_commits() as f64,
            })),
            None => Ok(None),
        }
    }

    /// Return info about the commit-graph if it is present (regardless of configuration), else `null`.
    #[napi]
    pub fn commit_graph(&self) -> Result<Option<CommitGraphInfo>> {
        let r = thread_local(&self.inner);
        match r.commit_graph() {
            Ok(graph) => Ok(Some(CommitGraphInfo {
                commit_count: graph.num_commits() as f64,
            })),
            Err(e) if e.is_not_found() => Ok(None),
            Err(e) => Err(err("commit_graph", e)),
        }
    }

    /// Return the full commit-graph if it is present *and* enabled, else `null`.
    #[napi]
    pub fn graph_if_enabled(&self) -> Result<Option<Graph>> {
        let r = thread_local(&self.inner);
        match r
            .commit_graph_if_enabled()
            .map_err(|e| err("commit_graph_if_enabled", e))?
        {
            Some(graph) => Ok(Some(Graph {
                inner: Arc::new(graph),
            })),
            None => Ok(None),
        }
    }

    /// Return the full commit-graph if it is present (regardless of configuration), else `null`.
    #[napi]
    pub fn graph(&self) -> Result<Option<Graph>> {
        let r = thread_local(&self.inner);
        match r.commit_graph() {
            Ok(graph) => Ok(Some(Graph {
                inner: Arc::new(graph),
            })),
            Err(e) if e.is_not_found() => Ok(None),
            Err(e) => Err(err("commit_graph", e)),
        }
    }
}

/// A [`gix_object::Find`](gix::objs::Find) adapter that resolves objects from a repository.
///
/// It owns an `Arc<gix::ThreadSafeRepository>` so the borrow it hands out is tied only to the
/// caller-provided buffer, never to a temporary `&Repository`.
struct RepoFind {
    repo: Arc<gix::ThreadSafeRepository>,
}

impl gix::objs::Find for RepoFind {
    fn try_find<'a>(
        &self,
        id: &gix::hash::oid,
        buffer: &'a mut Vec<u8>,
    ) -> gix::Result<Option<gix::objs::Data<'a>>> {
        thread_local(&self.repo).try_find(id, buffer)
    }
}

/// A mutable, in-memory commit-traversal graph that can associate data with commits.
///
/// This wraps `gix::revwalk::Graph` with `T = ()` (no per-commit data). It caches which commits have
/// been seen during a traversal and can optionally be accelerated by a [`Graph`] (commit-graph).
///
/// Note: borrow-returning methods of the underlying type (`lookup`, `get_or_insert_commit`, `get`)
/// are not exposed because they cannot cross the FFI boundary.
#[napi]
pub struct TraversalGraph {
    // Declared before `inner`: it holds the `Arc` behind `inner`'s `cache` reference, so it must be
    // dropped after `inner` (Rust drops fields in declaration order).
    cache: Option<Arc<gix::commitgraph::Graph>>,
    inner: gix::revwalk::Graph<'static, 'static, ()>,
}

#[napi]
impl TraversalGraph {
    /// Create a new traversal graph backed by `repo`'s object database.
    ///
    /// `cache` is an optional commit-graph (from [`Repository::graph`]) used to accelerate lookups.
    #[napi(constructor)]
    pub fn new(repo: &Repository, cache: Option<&Graph>) -> Result<Self> {
        let cache_arc = cache.map(|g| Arc::clone(&g.inner));
        // SAFETY: the `Arc` is stored in `self.cache` (declared above `inner`) and is only dropped
        // after `inner`, so the `Graph` it points to outlives `inner`'s `cache` reference.
        let cache_ref: Option<&'static gix::commitgraph::Graph> = cache_arc
            .as_ref()
            .map(|arc| unsafe { &*(Arc::as_ptr(arc) as *const gix::commitgraph::Graph) });
        let inner = gix::revwalk::Graph::new(
            RepoFind {
                repo: Arc::clone(&repo.inner),
            },
            cache_ref,
        );
        Ok(Self {
            cache: cache_arc,
            inner,
        })
    }

    /// The number of commits currently cached in this graph.
    #[napi(getter)]
    pub fn len(&self) -> f64 {
        self.inner.len() as f64
    }

    /// Whether no commit has been cached yet.
    #[napi]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Whether `id` has been cached (i.e. already processed).
    #[napi]
    pub fn contains(&self, id: String) -> Result<bool> {
        let oid = parse_oid(&id)?;
        Ok(self.inner.contains(oid.as_ref()))
    }

    /// Insert the parents of the commit named `id` into the graph.
    ///
    /// New parents are cached with default (`()`) data; already-cached parents are left untouched.
    /// If `first_parent` is `true`, only the first parent is considered (useful for first-parent walks).
    #[napi]
    pub fn insert_parents(&mut self, id: String, first_parent: bool) -> Result<()> {
        let oid = parse_oid(&id)?;
        let mut new_parent_data = |_id, _ts| ();
        let mut update_existing = |_id, _data: &mut ()| {};
        self.inner
            .insert_parents(
                oid.as_ref(),
                &mut new_parent_data,
                &mut update_existing,
                first_parent,
            )
            .map_err(|e| err("insert_parents", e))?;
        Ok(())
    }

    /// Remove all cached commits, starting the traversal over.
    #[napi]
    pub fn clear(&mut self) {
        self.inner.clear();
    }
}
