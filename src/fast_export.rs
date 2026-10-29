use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;

use gix::bstr::BString;
use napi::bindgen_prelude::{AsyncTask, Buffer};
use napi::{Env, Task};
use napi_derive::napi;

use crate::error::{err, err_str, parse_oid, thread_local};
use crate::fast_import::{ident_line, io_err, peel_to_commit, quote_path, short_tag_name};
use crate::repository::{Repository, lossy};

/// How signatures on commits and tags are handled (`--signed-commits`,
/// `--signed-tags`).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum SignMode {
    /// Export the signature verbatim.
    Verbatim,
    /// Drop the signature; this is git's default for commits.
    #[default]
    Strip,
    /// Fail when a signature is encountered.
    Abort,
}

impl SignMode {
    fn parse(text: &str, what: &str) -> napi::Result<Self> {
        match text {
            "verbatim" | "warn-verbatim" | "warn" => Ok(SignMode::Verbatim),
            "strip" | "warn-strip" => Ok(SignMode::Strip),
            "abort" => Ok(SignMode::Abort),
            // git's parse_opt_sign_mode rejects these for fast-export too:
            // verifying signatures lives in the git binary.
            other => Err(err_str(what, format!("unknown {what} mode: {other}"))),
        }
    }
}

/// What to do when a tag points at an object that was not exported
/// (`--tag-of-filtered-object`).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum TagOfFiltered {
    /// Fail; git's default.
    #[default]
    Abort,
    /// Omit the tag.
    Drop,
    /// Point the tag at the closest rewritten ancestor, which — without path
    /// filtering — is the tag's own target referenced by object id.
    Rewrite,
}

impl TagOfFiltered {
    fn parse(text: &str) -> napi::Result<Self> {
        match text {
            "abort" => Ok(TagOfFiltered::Abort),
            "drop" => Ok(TagOfFiltered::Drop),
            "rewrite" => Ok(TagOfFiltered::Rewrite),
            other => Err(err_str(
                "tagOfFilteredObject",
                format!("unknown tag-of-filtered mode: {other}"),
            )),
        }
    }
}

/// How a commit `encoding` header is handled (`--reencode`). Accepts git's
/// boolean spellings plus `abort` (case-insensitive).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum ReencodeMode {
    /// Fail on commits that declare an encoding; git's default.
    #[default]
    Abort,
    /// Emit the header verbatim and keep the bytes as stored.
    No,
    /// Transcode the message to UTF-8. Requires an iconv-style converter, so
    /// parsing it only exists to produce git's error message.
    Yes,
}

impl ReencodeMode {
    fn parse(text: &str) -> napi::Result<Self> {
        match text.to_ascii_lowercase().as_str() {
            "abort" => Ok(ReencodeMode::Abort),
            "no" | "false" | "0" | "off" => Ok(ReencodeMode::No),
            "yes" | "true" | "1" | "on" => Ok(ReencodeMode::Yes),
            other => Err(err_str(
                "reencode",
                format!("unknown reencoding mode: {other}"),
            )),
        }
    }
}

/// Per-export flags, unpacked once from [`FastExportOptions`].
#[derive(Clone, Copy)]
struct ExportFlags {
    no_data: bool,
    full_tree: bool,
    detect_renames: bool,
    find_copies: bool,
    use_done_feature: bool,
    mark_tags: bool,
    show_original_ids: bool,
    fake_missing_tagger: bool,
    max_commits: Option<usize>,
    progress_interval: Option<usize>,
    signed_commits: SignMode,
    signed_tags: SignMode,
    reencode: ReencodeMode,
    tag_of_filtered: TagOfFiltered,
    reference_excluded_parents: bool,
    anonymize: bool,
}

/// One `--refspec <src>:<dst>` mapping applied to exported ref names. A `*`
/// on both sides turns the refspec into a glob.
#[derive(Clone, Debug)]
struct Refspec {
    src: String,
    dst: String,
}

impl Refspec {
    fn parse(text: &str) -> napi::Result<Self> {
        let (src, dst) = text
            .split_once(':')
            .ok_or_else(|| err_str("refspec", format!("invalid refspec '{text}': missing ':'")))?;
        Ok(Refspec {
            src: src.to_string(),
            dst: dst.to_string(),
        })
    }

    /// Rewrite `name` through this refspec, or `None` when it does not match.
    fn apply(&self, name: &str) -> Option<String> {
        match (self.src.split_once('*'), self.dst.split_once('*')) {
            (Some((src_prefix, src_suffix)), Some((dst_prefix, dst_suffix))) => {
                let rest = name
                    .strip_prefix(src_prefix)
                    .filter(|rest| rest.ends_with(src_suffix) && rest.len() >= src_suffix.len())?;
                let middle = &rest[..rest.len() - src_suffix.len()];
                Some(format!("{dst_prefix}{middle}{dst_suffix}"))
            }
            (None, None) if self.src == name => Some(self.dst.clone()),
            _ => None,
        }
    }
}

/// Apply every refspec to an exported ref name; unmatched names pass through,
/// like git's `apply_refspecs`.
fn apply_refspecs(refspecs: &[Refspec], name: &str) -> String {
    refspecs
        .iter()
        .find_map(|refspec| refspec.apply(name))
        .unwrap_or_else(|| name.to_string())
}

/// Stateful generator behind `--anonymize`: every original string maps to a
/// stable replacement, seeded by `--anonymize-map` pairs. Mirrors git's
/// generator vocabulary (`ref%d`, `path%d`, `User %d <user%d@example.com>`,
/// `tag message %d`, `anonymous blob %d`, `subject %d`) so output shape stays
/// comparable, while timestamps are kept intact.
struct Anonymizer {
    seeds: Vec<(String, String)>,
    cache: HashMap<(u8, String), String>,
    /// Hex length of the repository's object ids.
    hexsz: usize,
    ref_counter: usize,
    path_counter: usize,
    ident_counter: usize,
    tag_counter: usize,
    blob_counter: usize,
    message_counter: usize,
    oid_counter: u32,
}

impl Anonymizer {
    fn new(seeds: Vec<(String, String)>, hexsz: usize) -> Self {
        Anonymizer {
            seeds,
            cache: HashMap::new(),
            hexsz,
            ref_counter: 0,
            path_counter: 0,
            ident_counter: 0,
            tag_counter: 0,
            blob_counter: 0,
            message_counter: 0,
            // git avoids handing out the null oid.
            oid_counter: 1,
        }
    }

    /// Seeds first, then the per-context cache, then a fresh mapping.
    fn intern(&mut self, kind: u8, orig: &str) -> String {
        if let Some((_, to)) = self.seeds.iter().find(|(from, _)| from == orig) {
            return to.clone();
        }
        if let Some(anon) = self.cache.get(&(kind, orig.to_string())) {
            return anon.clone();
        }
        let anon = match kind {
            b'r' => {
                self.ref_counter += 1;
                format!("ref{}", self.ref_counter - 1)
            }
            b'p' => {
                self.path_counter += 1;
                format!("path{}", self.path_counter - 1)
            }
            b'i' => {
                self.ident_counter += 1;
                format!(
                    "User {} <user{}@example.com>",
                    self.ident_counter - 1,
                    self.ident_counter - 1
                )
            }
            b't' => {
                self.tag_counter += 1;
                format!("tag message {}", self.tag_counter - 1)
            }
            b'o' => {
                self.oid_counter += 1;
                // Fake oid: all zeros except a big-endian counter at the end.
                format!("{}{:08x}", "0".repeat(self.hexsz - 8), self.oid_counter - 1)
            }
            _ => unreachable!("unknown anonymization context"),
        };
        self.cache.insert((kind, orig.to_string()), anon.clone());
        anon
    }

    /// Anonymize each component of `path` separately, so related paths share
    /// a common root.
    fn anonymize_path(&mut self, path: &str, kind: u8) -> String {
        path.split('/')
            .map(|component| self.intern(kind, component))
            .collect::<Vec<_>>()
            .join("/")
    }

    /// Ref names keep their well-known prefix so tags remain tags.
    fn refname(&mut self, name: &str) -> String {
        for prefix in ["refs/heads/", "refs/tags/", "refs/remotes/", "refs/"] {
            if let Some(rest) = name.strip_prefix(prefix) {
                return format!("{prefix}{}", self.anonymize_path(rest, b'r'));
            }
        }
        self.anonymize_path(name, b'r')
    }

    fn path(&mut self, path: &str) -> String {
        self.anonymize_path(path, b'p')
    }

    /// Replace `Name <email>` in a raw ident line, keeping the header word and
    /// the timestamp — traversal order depends on it.
    fn ident(&mut self, line: &str) -> String {
        let Some(space) = line.find(' ') else {
            return "Malformed Ident <malformed@example.com> 0 -0000".to_string();
        };
        let (header, rest) = line.split_at(space + 1);
        match (rest.find('<'), rest.find('>')) {
            (Some(start), Some(end)) if end > start => {
                let ident = &rest[..end + 1];
                let date = rest[end + 1..].trim();
                format!("{header}{} {date}", self.intern(b'i', ident))
            }
            _ => format!("{header}Malformed Ident <malformed@example.com> 0 -0000"),
        }
    }

    fn tag_message(&mut self, message: &[u8]) -> Vec<u8> {
        self.intern(b't', &String::from_utf8_lossy(message))
            .into_bytes()
    }

    fn commit_message(&mut self) -> Vec<u8> {
        self.blob_counter += 1;
        self.message_counter += 1;
        format!("subject {}\n\nbody\n", self.message_counter - 1).into_bytes()
    }

    fn blob(&mut self) -> Vec<u8> {
        self.blob_counter += 1;
        format!("anonymous blob {}", self.blob_counter - 1).into_bytes()
    }

    fn oid(&mut self, hex: &str) -> String {
        self.intern(b'o', hex)
    }
}

/// Writes a fast-export stream for one repository.
struct Exporter<'r> {
    r: &'r gix::Repository,
    /// Assembled stream.
    out: Vec<u8>,
    /// Commit stanza under construction. `blob` is a top-level command, so the
    /// blobs a stanza needs are staged separately and flushed before it.
    stanza: Vec<u8>,
    blobs: Vec<u8>,
    /// Marks already assigned to commits. Blob marks are consumed by the
    /// stream but never recorded, and tag marks live in [`Self::tag_marks`]:
    /// `--export-marks` dumps commits only, exactly like git.
    marks: HashMap<gix::ObjectId, usize>,
    /// Marks assigned to tag objects when `markTags` is set. Needed to
    /// reference nested tags, and deliberately not dumped by
    /// `--export-marks` (git's exporter filters its marks table to commits).
    tag_marks: HashMap<gix::ObjectId, usize>,
    next_mark: usize,
    /// Highest mark loaded by `--import-marks`. `--export-marks` writes only
    /// when something new was marked, exactly like git.
    imported_max: usize,
    /// Objects emitted so far, for `progress` lines.
    objects: usize,
    flags: ExportFlags,
    refspecs: Vec<Refspec>,
    anonymizer: Anonymizer,
}

impl<'r> Exporter<'r> {
    fn new(r: &'r gix::Repository, flags: ExportFlags, refspecs: Vec<Refspec>) -> Self {
        let hexsz = r.object_hash().len_in_bytes() * 2;
        Exporter {
            r,
            out: Vec::new(),
            stanza: Vec::new(),
            blobs: Vec::new(),
            marks: HashMap::new(),
            tag_marks: HashMap::new(),
            next_mark: 1,
            imported_max: 0,
            objects: 0,
            flags,
            refspecs,
            anonymizer: Anonymizer::new(Vec::new(), hexsz),
        }
    }

    /// The ref name as it appears in the stream: refspec-mapped and,
    /// under `--anonymize`, component-anonymized.
    fn export_name(&mut self, name: &str) -> String {
        let mapped = apply_refspecs(&self.refspecs, name);
        if self.flags.anonymize {
            self.anonymizer.refname(&mapped)
        } else {
            mapped
        }
    }

    /// Load a marks file produced by a previous run (or by
    /// [`crate::fast_import`]).
    ///
    /// The file must exist and be readable. Only revisions are loaded,
    /// matching git: blob marks in the file are skipped, and every commit
    /// named by the file is treated as already exported, which is what makes
    /// incremental bidirectional exports work.
    fn import_marks(&mut self, path: &std::path::Path) -> napi::Result<()> {
        let contents = std::fs::read_to_string(path).map_err(|e| {
            err_str(
                "fast_export",
                format!("cannot read marks file '{}': {e}", path.display()),
            )
        })?;
        for line in contents.lines() {
            let mut parts = line.split(' ');
            let mark = parts.next().unwrap_or_default().trim_start_matches(':');
            let oid = parts.next().unwrap_or_default();
            if mark.is_empty() || oid.is_empty() {
                continue;
            }
            let mark: usize = mark
                .parse()
                .map_err(|_| err_str("fast_export", format!("invalid mark '{mark}'")))?;
            let id = parse_oid(oid)?;
            let object = self
                .r
                .find_object(id)
                .map_err(|_| err_str("fast_export", format!("object not found: {id}")))?;
            if object.kind != gix::object::Kind::Commit {
                continue;
            }
            self.marks.insert(id, mark);
            self.next_mark = self.next_mark.max(mark + 1);
            self.imported_max = self.imported_max.max(mark);
        }
        Ok(())
    }

    /// `--import-marks-if-exists`: a missing file is silently skipped.
    fn import_marks_if_exists(&mut self, path: &std::path::Path) -> napi::Result<()> {
        if !path.exists() {
            return Ok(());
        }
        self.import_marks(path)
    }

    /// Export every ref in `refs`, apply `--refspec` deletions, then any
    /// trailing `done`.
    fn run(&mut self, refs: &[ExportRef]) -> napi::Result<()> {
        if self.flags.use_done_feature {
            writeln!(self.out, "feature done").map_err(io_err)?;
        }
        for reference in refs {
            if reference.is_tag {
                self.tag(&reference.name, reference.tip)?;
            } else {
                self.branch(&reference.name, reference.tip, &reference.hidden)?;
            }
        }
        // Refspecs with an empty source delete their destination ref.
        let deletions: Vec<String> = self
            .refspecs
            .iter()
            .filter(|refspec| refspec.src.is_empty())
            .map(|refspec| refspec.dst.clone())
            .collect();
        for dst in deletions {
            self.reset_from(&dst, &self.null_oid())?;
        }
        if self.flags.use_done_feature {
            writeln!(self.out, "done").map_err(io_err)?;
        }
        Ok(())
    }

    fn null_oid(&self) -> String {
        "0".repeat(self.r.object_hash().len_in_bytes() * 2)
    }

    /// `reset <name>` + `from <target>` + a blank line.
    fn reset_from(&mut self, name: &str, target: &str) -> napi::Result<()> {
        writeln!(self.out, "reset {name}").map_err(io_err)?;
        writeln!(self.out, "from {target}").map_err(io_err)?;
        writeln!(self.out).map_err(io_err)
    }

    /// Export the commits reachable from a branch tip, then point `name` at them.
    fn branch(
        &mut self,
        name: &str,
        tip: gix::ObjectId,
        hidden: &[gix::ObjectId],
    ) -> napi::Result<()> {
        let Some(tip) = peel_to_commit(self.r, tip)? else {
            return Ok(());
        };
        // A tip exported earlier under a different ref is only referenced.
        for id in self.walk(tip, hidden)? {
            if self.marks.contains_key(&id) {
                continue;
            }
            let mark = self.next_mark;
            self.next_mark += 1;
            self.marks.insert(id, mark);
            self.commit(id, name, mark)?;
        }
        if let Some(mark) = self.marks.get(&tip).copied() {
            let name = self.export_name(name);
            return self.reset_from(&name, &format!(":{mark}"));
        }
        // Nothing of this ref's history was exported: git deletes the ref,
        // unless `--reference-excluded-parents` keeps it by object id.
        let name = self.export_name(name);
        if self.flags.reference_excluded_parents {
            self.reset_from(&name, &tip.to_string())
        } else {
            self.reset_from(&name, &self.null_oid())
        }
    }

    /// Commit ids reachable from `tip` but not from `hidden`, parents first.
    fn walk(
        &self,
        tip: gix::ObjectId,
        hidden: &[gix::ObjectId],
    ) -> napi::Result<Vec<gix::ObjectId>> {
        let mut platform = self.r.rev_walk(vec![tip]);
        if !hidden.is_empty() {
            platform = platform.with_hidden(hidden.to_vec());
        }
        let walk = platform.all().map_err(|e| err("rev_walk", e))?;
        let mut ids = Vec::new();
        for item in walk {
            let item = item.map_err(|e| err("rev_walk item", e))?;
            ids.push(item.id);
        }
        // The walk is children-first; reverse it so every commit follows its
        // parents, which the stream format requires.
        ids.reverse();
        if let Some(max) = self.flags.max_commits {
            ids.truncate(max);
        }
        Ok(ids)
    }

    /// Write one `commit` stanza: header, parents, then file commands.
    fn commit(&mut self, id: gix::ObjectId, ref_name: &str, mark: usize) -> napi::Result<()> {
        self.stanza.clear();
        self.blobs.clear();
        let commit = self.r.find_commit(id).map_err(|e| err("find_commit", e))?;
        let decoded = commit.decode().map_err(|e| err("decode", e))?;
        let tree = parse_oid(&lossy(decoded.tree))?;
        let parents = self.parents(&decoded)?;
        let ref_name = self.export_name(ref_name);

        // Git recreates the ref before writing a root commit, so an import
        // into a repository that already has it starts from an empty branch.
        if parents.is_empty() {
            writeln!(self.stanza, "reset {ref_name}").map_err(io_err)?;
        }
        writeln!(self.stanza, "commit {ref_name}").map_err(io_err)?;
        writeln!(self.stanza, "mark :{mark}").map_err(io_err)?;
        if self.flags.show_original_ids {
            writeln!(self.stanza, "original-oid {id}").map_err(io_err)?;
        }
        let author = decoded.author().map_err(|e| err("author", e))?;
        let author_line = ident_line(&author)?;
        let author_line = if self.flags.anonymize {
            self.anonymizer.ident(&author_line)
        } else {
            author_line
        };
        writeln!(self.stanza, "author {author_line}").map_err(io_err)?;
        let committer = decoded.committer().map_err(|e| err("committer", e))?;
        let committer_line = ident_line(&committer)?;
        let committer_line = if self.flags.anonymize {
            self.anonymizer.ident(&committer_line)
        } else {
            committer_line
        };
        writeln!(self.stanza, "committer {committer_line}").map_err(io_err)?;
        self.commit_signature(&commit)?;
        match (decoded.encoding, self.flags.reencode) {
            (Some(encoding), ReencodeMode::Abort) => {
                return Err(err_str(
                    "fast_export",
                    format!(
                        "commit {id} declares encoding '{}'; pass reencode: \"no\" to keep it",
                        lossy(encoding)
                    ),
                ));
            }
            (Some(encoding), ReencodeMode::No) => {
                writeln!(self.stanza, "encoding {}", lossy(encoding)).map_err(io_err)?
            }
            (Some(_), ReencodeMode::Yes) => {
                return Err(err_str(
                    "fast_export",
                    "re-encoding commit messages to UTF-8 is not implemented",
                ));
            }
            (None, _) => {}
        }
        let message: &[u8] = if self.flags.anonymize {
            &self.anonymizer.commit_message()
        } else {
            decoded.message
        };
        writeln!(self.stanza, "data {}", message.len()).map_err(io_err)?;
        self.stanza.write_all(message).map_err(io_err)?;
        writeln!(self.stanza).map_err(io_err)?;

        // Parents without a mark were excluded (negative refs / ranges); git
        // skips them unless `--reference-excluded-parents` names them by id.
        let mut first = true;
        for parent in &parents {
            let Some(reference) = self.mark_of(*parent) else {
                if !self.flags.reference_excluded_parents {
                    continue;
                }
                let hex = if self.flags.anonymize {
                    self.anonymizer.oid(&parent.to_string())
                } else {
                    parent.to_string()
                };
                if first {
                    writeln!(self.stanza, "from {hex}").map_err(io_err)?;
                } else {
                    writeln!(self.stanza, "merge {hex}").map_err(io_err)?;
                }
                first = false;
                continue;
            };
            if first {
                writeln!(self.stanza, "from :{reference}").map_err(io_err)?;
            } else {
                writeln!(self.stanza, "merge :{reference}").map_err(io_err)?;
            }
            first = false;
        }

        if self.flags.full_tree {
            // `--full-tree` replaces the delta with the complete file list,
            // which needs the branch cleared first.
            writeln!(self.stanza, "deleteall").map_err(io_err)?;
            let tree = self.r.find_tree(tree).map_err(|e| err("find_tree", e))?;
            self.tree_entries(&tree, Vec::new())?;
        } else {
            // Like git, diff against the first parent only when it is part of
            // the export; an excluded parent falls back to the root diff.
            let parent_tree = match parents.first() {
                Some(parent)
                    if self.marks.contains_key(parent) || self.flags.reference_excluded_parents =>
                {
                    self.r
                        .find_commit(*parent)
                        .map_err(|e| err("find_commit", e))?
                        .tree_id()
                        .map_err(|e| err("tree_id", e))?
                        .detach()
                }
                _ => gix::ObjectId::empty_tree(self.r.object_hash()),
            };
            let old = self
                .r
                .find_tree(parent_tree)
                .map_err(|e| err("find_tree", e))?;
            let new = self.r.find_tree(tree).map_err(|e| err("find_tree", e))?;
            let options = if self.flags.detect_renames {
                let mut options = gix::diff::Options::default();
                options.track_rewrites(Some(gix::diff::Rewrites {
                    copies: self
                        .flags
                        .find_copies
                        .then(gix::diff::rewrites::Copies::default),
                    percentage: None,
                    limit: 1000,
                    track_empty: false,
                }));
                Some(options)
            } else {
                None
            };
            let changes = self
                .r
                .diff_tree_to_tree(Some(&old), Some(&new), options)
                .map_err(|e| err("diff_tree_to_tree", e))?;
            for change in changes {
                match change {
                    gix::object::tree::diff::ChangeDetached::Addition {
                        location,
                        id,
                        entry_mode,
                        ..
                    }
                    | gix::object::tree::diff::ChangeDetached::Modification {
                        location,
                        id,
                        entry_mode,
                        ..
                    } => self.modify(&location, entry_mode.into(), id)?,
                    gix::object::tree::diff::ChangeDetached::Deletion { location, .. } => {
                        let path = lossy(&location);
                        let path = if self.flags.anonymize {
                            self.anonymizer.path(&path)
                        } else {
                            path
                        };
                        writeln!(self.stanza, "D {}", quote_path(&path)).map_err(io_err)?
                    }
                    gix::object::tree::diff::ChangeDetached::Rewrite {
                        location,
                        source_location,
                        id,
                        entry_mode,
                        ..
                    } => {
                        if self.flags.detect_renames {
                            writeln!(
                                self.stanza,
                                "R {} {}",
                                quote_path(&lossy(&source_location)),
                                quote_path(&lossy(&location))
                            )
                            .map_err(io_err)?;
                        }
                        self.modify(&location, entry_mode.into(), id)?;
                    }
                }
            }
        }
        writeln!(self.stanza).map_err(io_err)?;

        self.out.extend_from_slice(&self.blobs);
        self.out.extend_from_slice(&self.stanza);
        self.note_object()?;
        Ok(())
    }

    /// Emit the `gpgsig` stanza of a signed commit, per `signedCommits`.
    fn commit_signature(&mut self, commit: &gix::Commit<'_>) -> napi::Result<()> {
        let Some((signature, _signed_payload)) =
            commit.signature().map_err(|e| err("signature", e))?
        else {
            return Ok(());
        };
        match self.flags.signed_commits {
            SignMode::Strip => Ok(()),
            SignMode::Abort => Err(err_str(
                "fast_export",
                format!(
                    "encountered signed commit {}; pass signedCommits: \"verbatim\" or \"strip\"",
                    commit.id
                ),
            )),
            SignMode::Verbatim => {
                let algo = match self.r.object_hash() {
                    gix::hash::Kind::Sha256 => "sha256",
                    _ => "sha1",
                };
                let format = signature_format(&signature);
                writeln!(self.stanza, "gpgsig {algo} {format}").map_err(io_err)?;
                writeln!(self.stanza, "data {}", signature.len()).map_err(io_err)?;
                self.stanza.write_all(&signature).map_err(io_err)?;
                writeln!(self.stanza).map_err(io_err)
            }
        }
    }

    /// Decode the parents of a commit.
    fn parents(&self, decoded: &gix::objs::CommitRef<'_>) -> napi::Result<Vec<gix::ObjectId>> {
        Ok(decoded.parents().map(|parent| parent.to_owned()).collect())
    }

    /// Emit every file of a tree (`--full-tree`).
    fn tree_entries(&mut self, tree: &gix::Tree<'_>, prefix: Vec<u8>) -> napi::Result<()> {
        let decoded = tree.decode().map_err(|e| err("decode", e))?;
        for entry in &decoded.entries {
            let mut path = prefix.clone();
            path.extend_from_slice(entry.filename);
            if entry.mode.is_tree() {
                path.push(b'/');
                let subtree = self
                    .r
                    .find_tree(entry.oid.to_owned())
                    .map_err(|e| err("find_tree", e))?;
                self.tree_entries(&subtree, path)?;
                continue;
            }
            self.modify(
                &BString::from(path),
                entry.mode.into(),
                entry.oid.to_owned(),
            )?;
        }
        Ok(())
    }

    /// Emit `M <mode> <ref> <path>`, with a `blob` stanza unless `no_data`.
    fn modify(
        &mut self,
        path: &BString,
        kind: gix::object::tree::EntryKind,
        id: gix::ObjectId,
    ) -> napi::Result<()> {
        let mode = crate::entry::mode_string(kind);
        let path = lossy(path);
        // Under `--anonymize` each path component is replaced, and blob
        // contents become generated text.
        let path = if self.flags.anonymize {
            self.anonymizer.path(&path)
        } else {
            path
        };
        if kind == gix::object::tree::EntryKind::Tree {
            // Directories are implied by the files under them — git never
            // emits a `M 040000` line, and its tree object would not exist in
            // the repository the stream is imported into.
            return Ok(());
        }
        // Gitlinks are referenced by id; blobs, executables and symlinks carry
        // inline data unless `--no-data` suppresses it.
        if self.flags.no_data || kind == gix::object::tree::EntryKind::Commit {
            // Referenced by id; under `--anonymize` the id is replaced too.
            let hex = if self.flags.anonymize {
                self.anonymizer.oid(&id.to_string())
            } else {
                id.to_string()
            };
            writeln!(self.stanza, "M {mode} {hex} {}", quote_path(&path)).map_err(io_err)?;
            return Ok(());
        }
        let object = self.r.find_object(id).map_err(|e| err("find_object", e))?;
        let mark = self.next_mark;
        self.next_mark += 1;
        writeln!(self.blobs, "blob").map_err(io_err)?;
        writeln!(self.blobs, "mark :{mark}").map_err(io_err)?;
        if self.flags.show_original_ids {
            writeln!(self.blobs, "original-oid {id}").map_err(io_err)?;
        }
        let data = if self.flags.anonymize {
            self.anonymizer.blob()
        } else {
            object.data.clone()
        };
        writeln!(self.blobs, "data {}", data.len()).map_err(io_err)?;
        self.blobs.write_all(&data).map_err(io_err)?;
        writeln!(self.blobs).map_err(io_err)?;
        writeln!(self.stanza, "M {mode} :{mark} {}", quote_path(&path)).map_err(io_err)?;
        self.note_object()?;
        Ok(())
    }

    /// Emit a `tag` stanza for an annotated tag, or `reset` for a lightweight
    /// one. Tags of trees are omitted: the stream has no way to name a tree.
    fn tag(&mut self, name: &str, id: gix::ObjectId) -> napi::Result<()> {
        let object = self.r.find_object(id).map_err(|e| err("find_object", e))?;
        if object.kind != gix::object::Kind::Tag {
            return self.lightweight_tag(name, id);
        }
        let tag = gix::objs::TagRef::from_bytes(&object.data, self.r.object_hash())
            .map_err(|e| err("tag decode", e))?;
        let direct = parse_oid(&lossy(tag.target))?;

        // Peel through nested tags only to detect tags of trees, which cannot
        // be expressed and are omitted, exactly like git.
        let mut peeled = direct;
        loop {
            let object = self
                .r
                .find_object(peeled)
                .map_err(|e| err("find_object", e))?;
            if object.kind != gix::object::Kind::Tag {
                if object.kind == gix::object::Kind::Tree {
                    return Ok(());
                }
                break;
            }
            let inner = gix::objs::TagRef::from_bytes(&object.data, self.r.object_hash())
                .map_err(|e| err("tag decode", e))?;
            peeled = parse_oid(&lossy(inner.target))?;
        }

        let mut message: Vec<u8> = tag.message.to_vec();
        if let Some(offset) = tag_signature_offset(&message) {
            match self.flags.signed_tags {
                SignMode::Abort => {
                    return Err(err_str(
                        "fast_export",
                        format!("encountered signed tag {}; use signedTags to handle it", id),
                    ));
                }
                SignMode::Strip => message.truncate(offset),
                SignMode::Verbatim => {}
            }
        }
        if self.flags.anonymize {
            message = self.anonymizer.tag_message(&message);
        }

        // The mark of the direct target: the inner tag for nested tags, else
        // the commit. Nothing marked means the object was filtered out.
        let target_mark = self
            .mark_of(direct)
            .or_else(|| self.tag_marks.get(&direct).copied());
        if target_mark.is_none() {
            match self.flags.tag_of_filtered {
                TagOfFiltered::Abort => {
                    return Err(err_str(
                        "fast_export",
                        format!(
                            "tag {id} tags unexported object; use tagOfFilteredObject to handle it"
                        ),
                    ));
                }
                TagOfFiltered::Drop => return Ok(()),
                TagOfFiltered::Rewrite => {
                    // Without path filtering, the closest rewritten ancestor
                    // is the target itself, referenced by object id; nested
                    // tags still require `--mark-tags`.
                    let nested = self
                        .r
                        .find_object(direct)
                        .map(|object| object.kind)
                        .unwrap_or(gix::object::Kind::Commit)
                        == gix::object::Kind::Tag;
                    if nested && !self.flags.mark_tags {
                        return Err(err_str(
                            "fast_export",
                            format!("cannot export nested tags unless markTags is set"),
                        ));
                    }
                }
            }
        }

        let name = self.export_name(name);
        let short = short_tag_name(&name);
        if self
            .r
            .find_object(direct)
            .map(|object| object.kind)
            .unwrap_or(gix::object::Kind::Commit)
            == gix::object::Kind::Tag
        {
            // Git deletes the ref first and lets the `tag` stanza recreate it.
            self.reset_from(&name, &self.null_oid())?;
        }
        writeln!(self.out, "tag {short}").map_err(io_err)?;
        if self.flags.mark_tags {
            let mark = self.next_mark;
            self.next_mark += 1;
            // Recorded separately: git's `--export-marks` dumps commits only.
            self.tag_marks.insert(id, mark);
            writeln!(self.out, "mark :{mark}").map_err(io_err)?;
        }
        let from = match target_mark {
            Some(mark) => format!(":{mark}"),
            None => {
                let hex = direct.to_string();
                if self.flags.anonymize {
                    self.anonymizer.oid(&hex)
                } else {
                    hex
                }
            }
        };
        writeln!(self.out, "from {from}").map_err(io_err)?;
        if self.flags.show_original_ids {
            writeln!(self.out, "original-oid {id}").map_err(io_err)?;
        }
        // `TagRef::tagger` is the raw header value: `Name <email> <ts> <tz>`.
        let tagger = match tag.tagger {
            Some(tagger) => {
                let line = lossy(tagger);
                if self.flags.anonymize {
                    self.anonymizer.ident(&format!("tagger {line}"))
                } else {
                    format!("tagger {line}")
                }
            }
            None if self.flags.fake_missing_tagger => format!("tagger {}", self.fake_tagger()?),
            None => String::new(),
        };
        if !tagger.is_empty() {
            writeln!(self.out, "{tagger}").map_err(io_err)?;
        }
        writeln!(self.out, "data {}", message.len()).map_err(io_err)?;
        self.out.write_all(&message).map_err(io_err)?;
        writeln!(self.out).map_err(io_err)?;
        self.note_object()?;
        Ok(())
    }

    /// A lightweight tag is a `reset` to its commit. When that commit was not
    /// exported, git deletes the ref unless `--reference-excluded-parents`
    /// keeps it by object id.
    fn lightweight_tag(&mut self, name: &str, id: gix::ObjectId) -> napi::Result<()> {
        let Some(commit) = peel_to_commit(self.r, id)? else {
            return Ok(());
        };
        let name = self.export_name(name);
        match self.mark_of(commit) {
            Some(mark) => self.reset_from(&name, &format!(":{mark}")),
            None if self.flags.reference_excluded_parents => {
                self.reset_from(&name, &commit.to_string())
            }
            None => self.reset_from(&name, &self.null_oid()),
        }
    }

    /// Identity used when a tag has no tagger and `--fake-missing-tagger` was
    /// requested: the repository's committer, or a fixed anonymous identity.
    fn fake_tagger(&self) -> napi::Result<String> {
        match self.r.committer().and_then(|sig| sig.ok()) {
            Some(sig) => {
                let time = sig.time().map_err(|e| err("tagger time", e))?;
                Ok(format!(
                    "{} <{}> {}",
                    lossy(sig.name),
                    lossy(sig.email),
                    time
                ))
            }
            None => Ok("anonymous <anonymous@example.com> 0 +0000".to_string()),
        }
    }

    /// The mark of an already-exported revision, if any.
    fn mark_of(&self, id: gix::ObjectId) -> Option<usize> {
        self.marks.get(&id).copied()
    }

    /// Count an emitted object, emitting `progress` on every interval.
    fn note_object(&mut self) -> napi::Result<()> {
        self.objects += 1;
        if self
            .flags
            .progress_interval
            .is_some_and(|every| every > 0 && self.objects % every == 0)
        {
            writeln!(self.out, "progress {} objects", self.objects).map_err(io_err)?;
        }
        Ok(())
    }

    /// Dump the marks table, one `:<mark> <oid>` per line, revisions only.
    ///
    /// Like git, the file is opened and truncated only when something new was
    /// actually exported: when the run marked nothing beyond what
    /// `--import-marks` loaded, the file is left untouched. That makes it safe
    /// to pass the same path to both options for incremental runs.
    fn export_marks(&self, path: &std::path::Path) -> napi::Result<()> {
        if self.next_mark - 1 <= self.imported_max {
            return Ok(());
        }
        let mut marks: Vec<(&gix::ObjectId, &usize)> = self.marks.iter().collect();
        marks.sort_by_key(|(_, mark)| **mark);
        let mut contents = String::new();
        for (id, mark) in marks {
            contents.push_str(&format!(":{mark} {id}\n"));
        }
        std::fs::write(path, contents).map_err(|e| {
            err_str(
                "fast_export",
                format!("cannot write marks file '{}': {e}", path.display()),
            )
        })
    }
}

/// Guess the signature format of a `gpgsig` payload, the way git's
/// `get_signature_format` does: by looking at the armor header.
fn signature_format(signature: &[u8]) -> &'static str {
    const MARKERS: &[(&[u8], &str)] = &[
        (b"-----BEGIN PGP SIGNATURE-----", "openpgp"),
        (b"-----BEGIN PGP MESSAGE-----", "openpgp"),
        (b"-----BEGIN SIGNED MESSAGE-----", "x509"),
        (b"-----BEGIN SSH SIGNATURE-----", "ssh"),
    ];
    for (marker, format) in MARKERS {
        if signature
            .windows(marker.len())
            .any(|window| window == *marker)
        {
            return format;
        }
    }
    "unknown"
}

/// Offset at which a tag message's signature begins, if it has one.
fn tag_signature_offset(message: &[u8]) -> Option<usize> {
    const MARKERS: &[&[u8]] = &[
        b"-----BEGIN PGP SIGNATURE-----",
        b"-----BEGIN PGP MESSAGE-----",
        b"-----BEGIN SIGNED MESSAGE-----",
        b"-----BEGIN SSH SIGNATURE-----",
    ];
    MARKERS
        .iter()
        .filter_map(|marker| {
            message
                .windows(marker.len())
                .position(|window| window == *marker)
        })
        .min()
}

/// Serialize `r` into a fast-export stream.
pub(crate) fn compute_fast_export(
    r: &gix::Repository,
    options: Option<&FastExportOptions>,
) -> napi::Result<Vec<u8>> {
    let options = options.cloned().unwrap_or_default();
    let anonymize = options.anonymize.unwrap_or(false);
    if options.anonymize_map.is_some() && !anonymize {
        return Err(err_str(
            "anonymizeMap",
            "the option 'anonymizeMap' requires 'anonymize'",
        ));
    }
    if options.import_marks.is_some() && options.import_marks_if_exists.is_some() {
        return Err(err_str(
            "fast_export",
            "options 'importMarks' and 'importMarksIfExists' cannot be used together",
        ));
    }
    let mut refspecs = Vec::new();
    for text in options.refspec.iter().flatten() {
        refspecs.push(Refspec::parse(text)?);
    }
    let flags = ExportFlags {
        no_data: options.no_data.unwrap_or(false),
        full_tree: options.full_tree.unwrap_or(false),
        detect_renames: options.detect_renames.unwrap_or(false),
        find_copies: options.find_copies.unwrap_or(false),
        use_done_feature: options.use_done_feature.unwrap_or(false),
        mark_tags: options.mark_tags.unwrap_or(false),
        show_original_ids: options.show_original_ids.unwrap_or(false),
        fake_missing_tagger: options.fake_missing_tagger.unwrap_or(false),
        max_commits: options.max_commits.map(|n| n as usize),
        progress_interval: options.progress_interval.map(|n| n as usize),
        signed_commits: match options.signed_commits.as_deref() {
            Some(mode) => SignMode::parse(mode, "signedCommits")?,
            None => SignMode::Strip,
        },
        signed_tags: match options.signed_tags.as_deref() {
            Some(mode) => SignMode::parse(mode, "signedTags")?,
            None => SignMode::Abort,
        },
        reencode: match options.reencode.as_deref() {
            None => ReencodeMode::Abort,
            Some(text) => match ReencodeMode::parse(text)? {
                ReencodeMode::Yes => {
                    return Err(err_str(
                        "reencode",
                        "re-encoding commit messages to UTF-8 is not implemented; pass reencode: \"no\" or \"abort\"",
                    ));
                }
                mode => mode,
            },
        },
        tag_of_filtered: match options.tag_of_filtered_object.as_deref() {
            Some(mode) => TagOfFiltered::parse(mode)?,
            None => TagOfFiltered::Abort,
        },
        reference_excluded_parents: options.reference_excluded_parents.unwrap_or(false),
        anonymize,
    };
    let mut exporter = Exporter::new(r, flags, refspecs);
    if anonymize {
        let mut seeds = Vec::new();
        for pair in options.anonymize_map.iter().flatten() {
            let (from, to) = pair.split_once(':').unwrap_or((pair, pair));
            if from.is_empty() || to.is_empty() {
                return Err(err_str(
                    "anonymizeMap",
                    "anonymizeMap token cannot be empty",
                ));
            }
            seeds.push((from.to_string(), to.to_string()));
        }
        exporter.anonymizer.seeds = seeds;
    }
    let relative_marks = options.relative_marks.unwrap_or(false);
    for path in options.import_marks.iter().flatten() {
        exporter.import_marks(&marks_path(r, relative_marks, path))?;
    }
    if let Some(path) = options.import_marks_if_exists.as_deref() {
        exporter.import_marks_if_exists(&marks_path(r, relative_marks, path))?;
    }
    let refs = export_refs(r, options.revs.as_ref(), &exporter.refspecs)?;
    exporter.run(&refs)?;
    if let Some(path) = options.export_marks.as_deref() {
        let path = marks_path(r, relative_marks, path);
        ensure_parent_dir(&path)?;
        exporter.export_marks(&path)?;
    }
    Ok(exporter.out)
}

/// Marks the start of the reference table that
/// [`Repository::export_repository`](crate::fast_export::Repository::export_repository)
/// appends after the stream's `done`. Anything after it is ignored by
/// `git fast-import`, which stops at `done`.
pub(crate) const REPOSITORY_EXPORT_MARKER: &str = "# repository-export v1";

/// Options for [`Repository::export_repository`] / [`export_repository`].
///
/// A repository export is a fast-import stream that additionally records every
/// reference, because fast-export itself only walks local branches and tags:
/// remotes, notes, stash and HEAD live in the appended reference table.
#[derive(Clone, Default)]
#[napi(object)]
pub struct RepositoryExportOptions {
    /// Options for the fast-export part; omit to export every local branch and
    /// tag.
    pub fast_export: Option<FastExportOptions>,
    /// Append the reference table. Default `true`.
    pub include_refs: Option<bool>,
    /// Restrict the recorded references to names matching this glob
    /// (`*` and `?`).
    pub refs_glob: Option<String>,
}

/// Serialize a repository into a fast-import stream plus a reference table.
///
/// The stream half comes from [`compute_fast_export`]; the table records every
/// reference (`ref <name> <id>`, `sym <name> <target>`, `head <target>`) after
/// a `done` command, so the archive is still a valid fast-import stream on its
/// own and can be applied with [`crate::fast_import::compute_fast_import`]
/// alone — the table is what turns it into a *whole repository* transfer.
pub(crate) fn compute_repository_export(
    r: &gix::Repository,
    options: Option<&RepositoryExportOptions>,
) -> napi::Result<Vec<u8>> {
    let options = options.cloned().unwrap_or_default();
    let mut out = compute_fast_export(r, options.fast_export.as_ref())?;
    // The importer stops at `done`; make sure it is there even when the export
    // was produced without `--use-done-feature`.
    if !out.ends_with(b"done\n") {
        out.extend_from_slice(b"done\n");
    }
    if !options.include_refs.unwrap_or(true) {
        return Ok(out);
    }
    writeln!(out, "{REPOSITORY_EXPORT_MARKER}").map_err(io_err)?;
    // `HEAD` as a symbolic ref; "-" marks a detached or unborn HEAD.
    let head = match r.head_ref().map_err(|e| err("head_ref", e))? {
        Some(reference) => reference.name().to_string(),
        None => "-".to_string(),
    };
    writeln!(out, "head {head}").map_err(io_err)?;

    let platform = r.references().map_err(|e| err("references", e))?;
    let mut entries: Vec<(String, String, bool)> = Vec::new();
    for reference in platform.all().map_err(|e| err("references", e))? {
        let reference = reference.map_err(|e| err("reference", e))?;
        let detached = reference.detach();
        let name = detached.name.to_string();
        if let Some(glob) = options.refs_glob.as_deref() {
            if !crate::ref_store::glob_match(glob, &name) {
                continue;
            }
        }
        match detached.target {
            gix::refs::Target::Object(id) => entries.push((name, id.to_string(), false)),
            gix::refs::Target::Symbolic(target) => entries.push((name, target.to_string(), true)),
        }
    }
    entries.sort();
    for (name, target, symbolic) in entries {
        if symbolic {
            writeln!(out, "sym {name} {target}").map_err(io_err)?;
        } else {
            writeln!(out, "ref {name} {target}").map_err(io_err)?;
        }
    }
    Ok(out)
}

/// One ref to export: its name, its tip, the commits to exclude (from a
/// `A..B` range), and whether it is a tag.
struct ExportRef {
    name: String,
    tip: gix::ObjectId,
    hidden: Vec<gix::ObjectId>,
    is_tag: bool,
}

/// The refs to export: explicit revisions, or every local branch and tag.
/// Names are rewritten through `refspecs` (`--refspec`); whether a ref is a
/// tag is decided from its original name, like git.
fn export_refs(
    r: &gix::Repository,
    revs: Option<&Vec<String>>,
    refspecs: &[Refspec],
) -> napi::Result<Vec<ExportRef>> {
    let mut branches: Vec<ExportRef> = Vec::new();
    let mut tags: Vec<ExportRef> = Vec::new();
    let mut push = |mut resolved: ExportRef| -> napi::Result<()> {
        resolved.is_tag = resolved.name.starts_with("refs/tags/");
        resolved.name = apply_refspecs(refspecs, &resolved.name);
        if resolved.is_tag {
            tags.push(resolved);
        } else {
            branches.push(resolved);
        }
        Ok(())
    };
    match revs {
        Some(revs) if !revs.is_empty() => {
            for rev in revs {
                push(resolve_export_rev(r, rev)?)?;
            }
        }
        _ => {
            let platform = r.references().map_err(|e| err("references", e))?;
            for reference in platform
                .local_branches()
                .map_err(|e| err("local_branches", e))?
            {
                let reference = reference.map_err(|e| err("reference", e))?;
                if let Some((name, id)) = direct_target(reference) {
                    push(ExportRef {
                        name,
                        tip: id,
                        hidden: Vec::new(),
                        is_tag: false,
                    })?;
                }
            }
            for reference in platform.tags().map_err(|e| err("tags", e))? {
                let reference = reference.map_err(|e| err("tag", e))?;
                if let Some((name, id)) = direct_target(reference) {
                    push(ExportRef {
                        name,
                        tip: id,
                        hidden: Vec::new(),
                        is_tag: true,
                    })?;
                }
            }
        }
    }
    branches.sort_by(|a, b| a.name.cmp(&b.name));
    tags.sort_by(|a, b| a.name.cmp(&b.name));
    branches.extend(tags);
    Ok(branches)
}

/// Name and target of a direct reference; `None` for symbolic ones.
fn direct_target(reference: gix::Reference) -> Option<(String, gix::ObjectId)> {
    let detached = reference.detach();
    match detached.target {
        gix::refs::Target::Object(id) => Some((detached.name.to_string(), id)),
        gix::refs::Target::Symbolic(_) => None,
    }
}

/// Resolve one user-supplied revision. Range syntax (`A..B`) exports `B` while
/// hiding everything reachable from `A`.
fn resolve_export_rev(r: &gix::Repository, rev: &str) -> napi::Result<ExportRef> {
    if let Some((excluded, included)) = rev.split_once("..") {
        if excluded.is_empty() || included.is_empty() || included.contains("..") {
            return Err(err_str("revs", format!("unsupported revision '{rev}'")));
        }
        let hidden = vec![
            r.rev_parse_single(excluded)
                .map_err(|e| err("rev_parse", e))?
                .detach(),
        ];
        let resolved = resolve_export_rev(r, included)?;
        return Ok(ExportRef { hidden, ..resolved });
    }
    if rev.starts_with("refs/") {
        let id = r
            .rev_parse_single(rev)
            .map_err(|e| err("rev_parse", e))?
            .detach();
        return Ok(ExportRef {
            name: rev.to_string(),
            tip: id,
            hidden: Vec::new(),
            is_tag: rev.starts_with("refs/tags/"),
        });
    }
    for candidate in [format!("refs/heads/{rev}"), format!("refs/tags/{rev}")] {
        if let Ok(reference) = r.find_reference(candidate.as_str()) {
            if let Some((name, id)) = direct_target(reference) {
                let is_tag = name.starts_with("refs/tags/");
                return Ok(ExportRef {
                    name,
                    tip: id,
                    hidden: Vec::new(),
                    is_tag,
                });
            }
        }
    }
    let id = r
        .rev_parse_single(rev)
        .map_err(|e| err("rev_parse", e))?
        .detach();
    Ok(ExportRef {
        name: format!("refs/heads/{rev}"),
        tip: id,
        hidden: Vec::new(),
        is_tag: false,
    })
}

// ---------------------------------------------------------------------------
// napi boundary
// ---------------------------------------------------------------------------

/// Options for [`fast_export`] and [`Repository::fast_export`].
///
/// Every field mirrors the `git fast-export` option of the same name; omitted
/// fields follow git's defaults (full stream with blobs, every local branch and
/// tag, no rename detection, no `done`).
#[derive(Clone, Default)]
#[napi(object)]
pub struct FastExportOptions {
    /// Revisions to export (branch names, tags, or revision expressions).
    /// Defaults to every local branch and tag.
    pub revs: Option<Vec<String>>,
    /// Emit `M <mode> <oid> <path>` instead of blob stanzas (`--no-data`). The
    /// result is only importable into a repository that already holds the blobs.
    pub no_data: Option<bool>,
    /// Emit `deleteall` plus every file of each commit instead of a delta
    /// against the first parent (`--full-tree`).
    pub full_tree: Option<bool>,
    /// Emit `R` commands for renames (`-M`).
    pub detect_renames: Option<bool>,
    /// Also detect copies and emit `C` commands (`-C`). Only meaningful when
    /// `detectRenames` is set.
    pub find_copies: Option<bool>,
    /// Wrap the stream in `feature done` / `done` (`--use-done-feature`).
    pub use_done_feature: Option<bool>,
    /// Emit a `mark` inside every `tag` stanza (`--mark-tags`).
    pub mark_tags: Option<bool>,
    /// Maximum number of commits emitted per reference (`null` = unlimited).
    pub max_commits: Option<u32>,
    /// Write the revision marks table here on completion (`--export-marks`).
    pub export_marks: Option<String>,
    /// Load marks from these files first; commits they name are not exported
    /// again (`--import-marks`, repeatable) — the basis of incremental exports.
    pub import_marks: Option<Vec<String>>,
    /// Add `original-oid <sha>` to commits and blobs (`--show-original-ids`).
    pub show_original_ids: Option<bool>,
    /// Insert a `progress` line every this many objects (`--progress=<n>`).
    pub progress_interval: Option<u32>,
    /// Synthesize a tagger for tags that have none (`--fake-missing-tagger`).
    pub fake_missing_tagger: Option<bool>,
    /// How to treat signed commits: `"verbatim"`, `"strip"` (default, git's
    /// default) or `"abort"` (`--signed-commits`). `warn-*` spellings are
    /// accepted and behave like their base mode.
    pub signed_commits: Option<String>,
    /// How to treat signed tags: `"verbatim"`, `"strip"` or `"abort"`
    /// (default, git's default) (`--signed-tags`).
    pub signed_tags: Option<String>,
    /// `"abort"` (default, git's default) rejects commits that declare an
    /// `encoding` header; `"no"` emits the header verbatim and keeps the
    /// message bytes as stored (`--reencode`). Accepts git's boolean
    /// spellings; `"yes"` (transcoding to UTF-8) is not implemented.
    pub reencode: Option<String>,
    /// What to do when a tag points at an object that was not exported:
    /// `"abort"` (default), `"drop"` or `"rewrite"` (`--tag-of-filtered-object`).
    /// Without path filtering, `"rewrite"` references the tag's own target by
    /// object id.
    pub tag_of_filtered_object: Option<String>,
    /// Reference commits that are not in the stream by object id instead of
    /// skipping them as `from`/`merge` targets (`--reference-excluded-parents`).
    pub reference_excluded_parents: Option<bool>,
    /// Replace names, emails, paths, ref names, messages, blob contents and
    /// object ids with stable fakes (`--anonymize`).
    pub anonymize: Option<bool>,
    /// `from:to` pairs used verbatim in anonymized output
    /// (`--anonymize-map`, repeatable). Requires `anonymize`.
    pub anonymize_map: Option<Vec<String>>,
    /// Rewrite exported ref names through fetch refspecs (`--refspec`,
    /// repeatable). Refspecs with an empty source delete their destination.
    pub refspec: Option<Vec<String>>,
    /// Read marks from this file if it exists (`--import-marks-if-exists`).
    /// Mutually exclusive with `importMarks`.
    pub import_marks_if_exists: Option<String>,
    /// Resolve every marks path relative to `<git-dir>/info/fast-import`,
    /// the way `git fast-export`/`git fast-import` do with
    /// `--relative-marks`. `false` spells `--no-relative-marks`; `null` (the
    /// default) leaves the paths as given.
    pub relative_marks: Option<bool>,
}

/// Resolve a marks file path.
///
/// With `relative` (git's `--relative-marks`) the path lives inside
/// `$GIT_DIR/info/fast-import`, the directory git keeps for incremental
/// imports; otherwise it is used verbatim.
pub(crate) fn marks_path(
    r: &gix::Repository,
    relative: bool,
    path: &str,
) -> std::path::PathBuf {
    if !relative {
        return std::path::PathBuf::from(path);
    }
    r.common_dir().join("info").join("fast-import").join(path)
}

/// Create the directory holding `path`, mirroring git, which creates
/// `info/fast-import` on demand before writing a relative marks file.
pub(crate) fn ensure_parent_dir(path: &std::path::Path) -> napi::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| err("fast_export", e))?;
        }
    }
    Ok(())
}

/// Open the repository at `dir` for a one-shot operation.
fn open_repository(dir: &str) -> napi::Result<gix::Repository> {
    let thread_safe = Arc::new(gix::ThreadSafeRepository::open(dir).map_err(|e| err("open", e))?);
    Ok(thread_local(&thread_safe))
}

/// Serialize the repository at `dir` into a fast-export stream.
///
/// The stream can be fed to [`crate::fast_import::fast_import`], edited in
/// between (that is how `git filter-repo` rewrites history), or handed to the
/// real `git fast-import`.
#[napi]
pub fn fast_export(dir: String, options: Option<FastExportOptions>) -> napi::Result<Buffer> {
    let r = open_repository(&dir)?;
    Ok(Buffer::from(compute_fast_export(&r, options.as_ref())?))
}

/// [`fast_export`] on the libuv thread pool.
#[napi(ts_return_type = "Promise<Buffer>")]
pub fn fast_export_async(
    dir: String,
    options: Option<FastExportOptions>,
) -> napi::Result<AsyncTask<FastExportTask>> {
    Ok(AsyncTask::new(FastExportTask { dir, options }))
}

/// [`fast_export`] as a [`Task`].
pub struct FastExportTask {
    dir: String,
    options: Option<FastExportOptions>,
}

impl Task for FastExportTask {
    type Output = Vec<u8>;
    type JsValue = Buffer;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        let r = open_repository(&self.dir)?;
        compute_fast_export(&r, self.options.as_ref())
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(Buffer::from(output))
    }
}

/// [`Repository::fast_export_async`] as a [`Task`].
pub struct RepositoryFastExportTask {
    repo: Arc<gix::ThreadSafeRepository>,
    options: Option<FastExportOptions>,
}

impl Task for RepositoryFastExportTask {
    type Output = Vec<u8>;
    type JsValue = Buffer;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        let r = thread_local(&self.repo);
        compute_fast_export(&r, self.options.as_ref())
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(Buffer::from(output))
    }
}

#[napi]
impl Repository {
    /// Serialize this repository into a fast-export stream.
    ///
    /// Defaults to every local branch and tag; pass `{ revs: ["main"] }` to
    /// limit it. The stream can be fed back with
    /// [`fast_import`](crate::fast_import::fast_import).
    #[napi]
    pub fn fast_export(&self, options: Option<FastExportOptions>) -> napi::Result<Buffer> {
        let r = thread_local(&self.inner);
        Ok(Buffer::from(compute_fast_export(&r, options.as_ref())?))
    }

    /// [`fast_export`](Self::fast_export) on the libuv thread pool.
    #[napi(ts_return_type = "Promise<Buffer>")]
    pub fn fast_export_async(
        &self,
        options: Option<FastExportOptions>,
    ) -> napi::Result<AsyncTask<RepositoryFastExportTask>> {
        Ok(AsyncTask::new(RepositoryFastExportTask {
            repo: Arc::clone(&self.inner),
            options,
        }))
    }

    /// Serialize the whole repository: a fast-import stream (history reachable
    /// from the exported refs) plus a table of every reference, including the
    /// ones fast-export does not walk — remotes, notes, stash and HEAD.
    ///
    /// Apply it with
    /// [`import_repository`](crate::fast_import::Repository::import_repository).
    /// The archive is also a plain fast-import stream, so
    /// `fast_import(archive)` alone reproduces the history and the local
    /// branches and tags. The worktree is not part of the archive; pass
    /// `{ checkout: true }` to the importer to recreate the files.
    #[napi]
    pub fn export_repository(
        &self,
        options: Option<RepositoryExportOptions>,
    ) -> napi::Result<Buffer> {
        let r = thread_local(&self.inner);
        Ok(Buffer::from(compute_repository_export(
            &r,
            options.as_ref(),
        )?))
    }
}

/// Serialize the repository at `dir` into a fast-import stream plus a
/// reference table. See
/// [`Repository::export_repository`](crate::fast_export::Repository::export_repository).
#[napi]
pub fn export_repository(
    dir: String,
    options: Option<RepositoryExportOptions>,
) -> napi::Result<Buffer> {
    let r = open_repository(&dir)?;
    Ok(Buffer::from(compute_repository_export(
        &r,
        options.as_ref(),
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Initialize a repository at `path` and return a thread-local view of it.
    fn init_repo(path: &std::path::Path) -> gix::Repository {
        let repo: gix::ThreadSafeRepository = gix::init(path).unwrap().into();
        thread_local(&Arc::new(repo))
    }

    /// A two-commit stream: `a.txt` in the first commit, `a.txt` edited and
    /// `nested/b.txt` added in the second.
    fn sample_stream() -> Vec<u8> {
        let mut out = Vec::new();
        let mut importer = crate::fast_import::GitFastImporter::new(
            &mut out,
            "main".to_string(),
            None,
            "committer Test <test@example.com>".to_string(),
        );
        let mut first = importer.start_commit("first", chrono::DateTime::UNIX_EPOCH);
        first.add_file("a.txt", b"one\n").unwrap();
        first.finish().unwrap();
        let mut second = importer.start_commit("second", chrono::DateTime::UNIX_EPOCH);
        second.add_file("a.txt", b"two\n").unwrap();
        second.add_file("nested/b.txt", b"bee\n").unwrap();
        second.finish().unwrap();
        importer.finish().unwrap();
        out
    }

    #[test]
    fn test_export_contains_commits_and_blobs() {
        let dir = tempfile::tempdir().unwrap();
        let r = init_repo(dir.path());
        crate::fast_import::compute_fast_import(&r, &sample_stream(), None).unwrap();

        let exported = compute_fast_export(&r, None).unwrap();
        let text = String::from_utf8(exported).unwrap();
        assert!(text.contains("commit refs/heads/main"), "{text}");
        assert!(text.contains("committer Test <test@example.com>"), "{text}");
        assert!(text.contains("M 100644 "), "{text}");
        assert!(text.contains("a.txt"), "{text}");
        assert!(text.contains("nested/b.txt"), "{text}");
        // Blobs precede the commit that references them.
        let blob = text.find("blob\n").unwrap();
        let commit = text.find("commit refs/heads/main").unwrap();
        assert!(blob < commit, "{text}");
    }

    #[test]
    fn test_export_no_data_references_blobs_by_id() {
        let dir = tempfile::tempdir().unwrap();
        let r = init_repo(dir.path());
        crate::fast_import::compute_fast_import(&r, &sample_stream(), None).unwrap();

        let exported = compute_fast_export(
            &r,
            Some(&FastExportOptions {
                no_data: Some(true),
                ..FastExportOptions::default()
            }),
        )
        .unwrap();
        let text = String::from_utf8(exported).unwrap();
        assert!(!text.contains("\nblob\n"), "{text}");
        assert!(text.contains("M 100644 "), "{text}");
    }

    #[test]
    fn test_export_full_tree_emits_every_file() {
        let dir = tempfile::tempdir().unwrap();
        let r = init_repo(dir.path());
        crate::fast_import::compute_fast_import(&r, &sample_stream(), None).unwrap();

        let exported = compute_fast_export(
            &r,
            Some(&FastExportOptions {
                full_tree: Some(true),
                use_done_feature: Some(true),
                ..FastExportOptions::default()
            }),
        )
        .unwrap();
        let text = String::from_utf8(exported).unwrap();
        assert!(text.starts_with("feature done\n"), "{text}");
        assert!(text.contains("deleteall"), "{text}");
        assert!(text.trim_end().ends_with("done"), "{text}");
        assert_eq!(text.matches("M 100644 ").count(), 3, "{text}");
    }

    #[test]
    fn test_export_marks_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let r = init_repo(dir.path());
        crate::fast_import::compute_fast_import(&r, &sample_stream(), None).unwrap();
        let marks_path = dir.path().join("marks");

        compute_fast_export(
            &r,
            Some(&FastExportOptions {
                export_marks: Some(marks_path.to_string_lossy().to_string()),
                ..FastExportOptions::default()
            }),
        )
        .unwrap();
        // Revision marks only: one line per exported commit, no blob marks.
        let dumped = std::fs::read_to_string(&marks_path).unwrap();
        assert_eq!(dumped.lines().count(), 2, "{dumped}");
        assert!(dumped.starts_with(":1 "), "{dumped}");

        // Importing those marks means nothing is exported again.
        let exported = compute_fast_export(
            &r,
            Some(&FastExportOptions {
                import_marks: Some(vec![marks_path.to_string_lossy().to_string()]),
                ..FastExportOptions::default()
            }),
        )
        .unwrap();
        let text = String::from_utf8(exported).unwrap();
        assert!(!text.contains("\ncommit "), "{text}");
        assert!(text.contains("reset refs/heads/main"), "{text}");
    }

    #[test]
    fn test_export_marks_untouched_when_nothing_new() {
        let dir = tempfile::tempdir().unwrap();
        let r = init_repo(dir.path());
        crate::fast_import::compute_fast_import(&r, &sample_stream(), None).unwrap();
        // The same path for both options is safe: export only rewrites the
        // file when the run marked something new.
        let marks_path = dir.path().join("marks");
        let path = marks_path.to_string_lossy().to_string();
        compute_fast_export(
            &r,
            Some(&FastExportOptions {
                export_marks: Some(path.clone()),
                ..FastExportOptions::default()
            }),
        )
        .unwrap();
        let first = std::fs::read_to_string(&marks_path).unwrap();

        // A fully imported run marks nothing new: the file is left alone.
        let copy = dir.path().join("marks.copy");
        std::fs::copy(&marks_path, &copy).unwrap();
        std::fs::remove_file(&marks_path).unwrap();
        compute_fast_export(
            &r,
            Some(&FastExportOptions {
                import_marks: Some(vec![copy.to_string_lossy().to_string()]),
                export_marks: Some(path),
                ..FastExportOptions::default()
            }),
        )
        .unwrap();
        assert!(!marks_path.exists());
        assert_eq!(first, std::fs::read_to_string(&copy).unwrap());
    }

    #[test]
    fn test_nested_tags_need_mark_tags() {
        let dir = tempfile::tempdir().unwrap();
        let r = init_repo(dir.path());
        // inner → second commit, outer → inner. The sample stream already
        // ends with `done`, which terminates the import, so reopen it.
        let mut stream = sample_stream();
        assert!(stream.ends_with(b"done\n"));
        stream.truncate(stream.len() - "done\n".len());
        stream.extend_from_slice(
            b"\
tag inner
from refs/heads/main
tagger Test <test@example.com> 0 +0000
data 6
inner

tag outer
from refs/tags/inner
tagger Test <test@example.com> 0 +0000
data 6
outer

done
",
        );
        crate::fast_import::compute_fast_import(&r, &stream, None).unwrap();

        // Without marks the inner tag has no mark, so the outer tag points at
        // an unexported object and git refuses (TAG_FILTERING_ABORT).
        let error = compute_fast_export(&r, None).err().unwrap();
        assert!(
            error.to_string().contains("tags unexported object"),
            "{error}"
        );
        // Asking for a rewrite names the real obstacle: nested tags.
        let error = compute_fast_export(
            &r,
            Some(&FastExportOptions {
                tag_of_filtered_object: Some("rewrite".to_string()),
                ..FastExportOptions::default()
            }),
        )
        .err()
        .unwrap();
        assert!(error.to_string().contains("nested tags"), "{error}");

        let exported = compute_fast_export(
            &r,
            Some(&FastExportOptions {
                mark_tags: Some(true),
                ..FastExportOptions::default()
            }),
        )
        .unwrap();
        // Git deletes the ref, then recreates it from the inner tag's mark.
        let text = String::from_utf8(exported.clone()).unwrap();
        assert!(text.contains("reset refs/tags/outer\nfrom 000"), "{text}");
        assert!(text.contains("tag outer\nmark :"), "{text}");
        assert!(text.contains("tag inner\nmark :"), "{text}");

        // The exported stream re-imports into a fresh repository.
        let target_dir = tempfile::tempdir().unwrap();
        let target = init_repo(target_dir.path());
        let stats = crate::fast_import::compute_fast_import(&target, &exported, None).unwrap();
        assert_eq!(stats.tags, 2);
        // The ref points at the tag object itself; peeling would give the commit.
        let outer = match target
            .find_reference("refs/tags/outer")
            .unwrap()
            .detach()
            .target
        {
            gix::refs::Target::Object(id) => id,
            gix::refs::Target::Symbolic(_) => panic!("refs/tags/outer is symbolic"),
        };
        let outer_tag = target.find_object(outer).unwrap();
        let decoded = gix::objs::TagRef::from_bytes(&outer_tag.data, target.object_hash()).unwrap();
        assert_eq!(decoded.target_kind, gix::object::Kind::Tag);
    }
}
