//! NAPI bindings for git's content-filter pipeline (smudge/clean, CRLF/EOL, attribute-driven
//! keyword expansion) that runs when moving bytes between the working tree and the object
//! database. This wraps [`gix_filter::Pipeline`] (the low-level primitive); each conversion
//! rebuilds the attribute cache and index lookup on the fly (cheap relative to the actual
//! filtering work) and drains the resulting stream into a [`Buffer`] for the FFI boundary.
use std::io::Read;

use napi::Result;
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use gix::bstr::BStr;
use gix::config::tree::Core;
use gix::objs::Find;

use gix_filter::driver::apply::MaybeDelayed;
use gix_filter::pipeline::convert::to_worktree::UnknownEncoding;
use gix_filter::pipeline::convert::{ToGitOutcome, ToWorktreeOutcome};

use crate::error::{err, err_str, thread_local};
use crate::repository::Repository;

/// A configured filter pipeline bound to a repository.
///
/// Construct it with the owning [`Repository`]; the underlying [`gix_filter::Pipeline`] (driver
/// state, buffers, and resolved options) is built once and reused across conversions. Each
/// conversion still rebuilds the per-path attribute lookup on the fly and drains the resulting
/// stream into a [`Buffer`] for the FFI boundary.
#[napi]
pub struct FilterPipeline {
    repo: Repository,
    pipeline: gix_filter::Pipeline,
}

#[napi]
impl FilterPipeline {
    /// Create a pipeline bound to `repo`. The underlying [`gix_filter::Pipeline`] (driver state,
    /// buffers, resolved options) is constructed once here and reused for every conversion.
    #[napi(constructor)]
    pub fn new(repo: &Repository) -> Result<Self> {
        let r = thread_local(&repo.inner);
        let options = build_options(&r)?;
        let context = r.command_context().map_err(|e| err("command_context", e))?;
        let pipeline = gix_filter::Pipeline::new(context, r.object_hash(), options);
        Ok(FilterPipeline {
            repo: Repository {
                inner: repo.inner.clone(),
            },
            pipeline,
        })
    }

    /// Convert `src` (located at repo-relative `rela_path`) from the working-tree representation
    /// to the representation stored in `git`. Returns the (possibly filtered) bytes.
    #[napi]
    pub fn convert_to_git(&mut self, src: Buffer, rela_path: String) -> Result<Buffer> {
        self.convert_to_git_read(src.as_ref(), &rela_path)
            .map(Buffer::from)
    }

    /// Convert `src` (located at repo-relative `rela_path`) from what is stored in `git` to the
    /// working-tree representation. Returns the (possibly filtered) bytes.
    ///
    /// - `can_delay` (default `false`): allow long-running process filters to defer their response.
    ///   When `true` and a filter delays, this polls the process until its output is available
    ///   (bounded by a 30s timeout that surfaces as an error). `false` forces an immediate result.
    /// - `unknown_encoding` (default `"ignore"`): `"fail"` surfaces an unknown `working-tree-encoding`
    ///   as an error instead of silently passing the bytes through.
    #[napi]
    pub fn convert_to_worktree(
        &mut self,
        src: Buffer,
        rela_path: String,
        can_delay: Option<bool>,
        unknown_encoding: Option<String>,
    ) -> Result<Buffer> {
        let r = thread_local(&self.repo.inner);
        let index = r.index_or_empty().map_err(|e| err("index_or_empty", e))?;
        let mut cache = r
            .attributes_only(
                &index,
                gix::worktree::stack::state::attributes::Source::WorktreeThenIdMapping,
            )
            .map_err(|e| err("attributes_only", e))?;
        let entry = cache
            .at_path(rela_path.as_str(), None)
            .map_err(|e| err("at_path", e))?;
        let bstr_rela_path: &gix::bstr::BStr = rela_path.as_str().into();
        let can_delay = if can_delay.unwrap_or(false) {
            gix_filter::driver::apply::Delay::Allow
        } else {
            gix_filter::driver::apply::Delay::Forbid
        };
        let unknown_encoding = match unknown_encoding.as_deref() {
            Some("fail") => UnknownEncoding::Fail,
            _ => UnknownEncoding::Ignore,
        };
        // Match the temporary directly so `outcome` (which borrows self.pipeline) is dropped at the
        // end of the match, releasing the borrow before the polling loop below re-borrows the pipeline.
        let key = match self
            .pipeline
            .convert_to_worktree(
                src.as_ref(),
                bstr_rela_path,
                &mut |_, out| {
                    entry.matching_attributes(out);
                },
                gix_filter::pipeline::convert::to_worktree::Options {
                    can_delay,
                    unknown_encoding,
                },
            )
            .map_err(|e| err("convert_to_worktree", e))?
        {
            ToWorktreeOutcome::Process(md) => match md {
                gix_filter::driver::apply::MaybeDelayed::Delayed(key) => key,
                gix_filter::driver::apply::MaybeDelayed::Immediate(mut r) => {
                    let mut out = Vec::new();
                    r.read_to_end(&mut out)
                        .map_err(|e| err("read_process_output", e))?;
                    return Ok(Buffer::from(out));
                }
            },
            other => return drain_worktree(other).map(Buffer::from),
        };
        // A long-running filter deferred this path. Poll the process until our path shows up
        // among the available blobs, then fetch the filtered result. The NAPI boundary is
        // synchronous, so this blocks the calling thread (bounded by a timeout).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let target: &[u8] = bstr_rela_path.as_ref();
        loop {
            let ready = self
                .pipeline
                .driver_state_mut()
                .list_delayed_paths(&key)
                .map_err(|e| err("list_delayed_paths", e))?;
            if ready.iter().any(|p| {
                let p_bytes: &[u8] = p.as_ref();
                p_bytes == target
            }) {
                let mut reader = self
                    .pipeline
                    .driver_state_mut()
                    .fetch_delayed(&key, bstr_rela_path, gix_filter::driver::Operation::Smudge)
                    .map_err(|e| err("fetch_delayed", e))?;
                let mut out = Vec::new();
                reader
                    .read_to_end(&mut out)
                    .map_err(|e| err("read_delayed_output", e))?;
                return Ok(Buffer::from(out));
            }
            if std::time::Instant::now() >= deadline {
                return Err(err_str(
                    "convert_to_worktree",
                    "timed out waiting for a delayed filter process to produce output",
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// Terminate any long-running filter processes (git-LFS, etc.) spawned by conversions so far,
    /// waiting for them to exit. Returns per-process outcomes. After this call the pipeline can
    /// launch fresh processes for subsequent conversions.
    #[napi]
    pub fn shutdown(&mut self) -> Result<FilterShutdownOutcome> {
        let out = self
            .pipeline
            .driver_state_mut()
            .shutdown(gix_filter::driver::shutdown::Mode::WaitForProcesses)
            .map_err(|e| err("shutdown", e))?;
        Ok(FilterShutdownOutcome {
            processes: out
                .processes
                .into_iter()
                .map(|(cmd, status)| FilterProcessOutcome {
                    command: cmd.to_string(),
                    exit_status: status.and_then(|s| s.code()),
                })
                .collect(),
        })
    }

    /// Return the filter drivers configured for this repository (from every `filter.*` section).
    #[napi]
    pub fn filter_drivers(&mut self) -> Vec<FilterDriver> {
        self.pipeline
            .options_mut()
            .drivers
            .iter()
            .map(|d| FilterDriver {
                name: d.name.to_string(),
                clean: d.clean.as_ref().map(|s| s.to_string()),
                smudge: d.smudge.as_ref().map(|s| s.to_string()),
                process: d.process.as_ref().map(|s| s.to_string()),
                required: d.required,
            })
            .collect()
    }

    /// Set the process-filter context passed to `process` filters as `ref`, `treeish`, and `blob`.
    /// Any argument left `undefined` resets that field to `None`. `treeish`/`blob` are hex object ids.
    #[napi]
    pub fn set_driver_context(
        &mut self,
        ref_name: Option<String>,
        treeish: Option<String>,
        blob: Option<String>,
    ) -> Result<()> {
        let ctx = self.pipeline.driver_context_mut();
        ctx.ref_name = ref_name.map(|s| gix::bstr::BString::from(s));
        ctx.treeish = match treeish {
            Some(s) => Some(
                gix::hash::ObjectId::from_hex(s.as_bytes())
                    .map_err(|e| err("set_driver_context", e))?,
            ),
            None => None,
        };
        ctx.blob = match blob {
            Some(s) => Some(
                gix::hash::ObjectId::from_hex(s.as_bytes())
                    .map_err(|e| err("set_driver_context", e))?,
            ),
            None => None,
        };
        Ok(())
    }

    /// Add the working-tree file at repo-relative `rela_path` to the object database, applying the
    /// to-git filters, and return its identity and entry kind. Returns `null` if the file does not
    /// exist or is of an untrackable type.
    ///
    /// Note: embedding a directory that is itself a repository (submodule) is deferred; directories
    /// currently return `null`.
    #[napi]
    pub fn worktree_file_to_object(
        &mut self,
        rela_path: String,
    ) -> Result<Option<FilterFileObject>> {
        let r = thread_local(&self.repo.inner);
        let worktree_dir = r.workdir().ok_or_else(|| {
            err_str(
                "worktree_file_to_object",
                "Cannot add worktree files in bare repositories",
            )
        })?;
        let path = worktree_dir.join(
            gix::path::from_bstr(BStr::new(rela_path.as_str())).map_err(|e| err("from_bstr", e))?,
        );
        let md = match std::fs::symlink_metadata(&path) {
            Ok(md) => md,
            Err(e) => {
                if gix::fs::io_err::is_not_found(e.kind(), e.raw_os_error()) {
                    return Ok(None);
                } else {
                    return Err(err("symlink_metadata", e));
                }
            }
        };
        let (id, kind) = if md.is_symlink() {
            let target = std::fs::read_link(&path).map_err(|e| err("read_link", e))?;
            let id = r
                .write_blob(
                    gix::path::into_bstr(target.as_path())
                        .map_err(|e| err("into_bstr", e))?
                        .as_ref(),
                )
                .map_err(|e| err("write_blob", e))?;
            (id, gix::objs::tree::EntryKind::Link)
        } else if md.is_file() {
            let file = std::fs::File::open(&path).map_err(|e| err("open", e))?;
            let bytes = self.convert_to_git_read(file, &rela_path)?;
            let id = r.write_blob(&bytes).map_err(|e| err("write_blob", e))?;
            let kind = if gix::fs::is_executable(&md) {
                gix::objs::tree::EntryKind::BlobExecutable
            } else {
                gix::objs::tree::EntryKind::Blob
            };
            (id, kind)
        } else {
            // Directories (including potential submodules) are not embedded yet.
            return Ok(None);
        };

        Ok(Some(FilterFileObject {
            id: id.to_string(),
            kind: format!("{:?}", kind),
            is_executable: gix::fs::is_executable(&md),
            is_symlink: md.is_symlink(),
            is_file: md.is_file(),
            is_dir: md.is_dir(),
        }))
    }

    /// Read the working-tree file at repo-relative `rela_path`, apply the to-git (clean) filters,
    /// and return the resulting bytes. This mirrors `git_filter_list_apply_to_file` in the
    /// `GIT_FILTER_TO_GIT` mode.
    ///
    /// Returns `null` if the file does not exist or is of an untrackable type (e.g. a directory).
    #[napi]
    pub fn convert_to_git_file(&mut self, rela_path: String) -> Result<Option<Buffer>> {
        let r = thread_local(&self.repo.inner);
        let worktree_dir = r.workdir().ok_or_else(|| {
            err_str(
                "convert_to_git_file",
                "Cannot read worktree files in bare repositories",
            )
        })?;
        let path = worktree_dir.join(
            gix::path::from_bstr(BStr::new(rela_path.as_str())).map_err(|e| err("from_bstr", e))?,
        );
        let file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(e) => {
                if gix::fs::io_err::is_not_found(e.kind(), e.raw_os_error()) {
                    return Ok(None);
                }
                return Err(err("open", e));
            }
        };
        let bytes = self.convert_to_git_read(file, &rela_path)?;
        Ok(Some(Buffer::from(bytes)))
    }

    /// Read the blob identified by `id`, apply the to-worktree (smudge) filters for `rela_path`,
    /// and return the resulting bytes. This mirrors `git_filter_list_apply_to_blob` in the
    /// `GIT_FILTER_TO_WORKTREE` mode and is the typical "check out a blob through filters" path.
    ///
    /// - `can_delay` / `unknown_encoding`: same semantics as [`FilterPipeline::convert_to_worktree`].
    #[napi]
    pub fn convert_to_worktree_blob(
        &mut self,
        id: String,
        rela_path: String,
        can_delay: Option<bool>,
        unknown_encoding: Option<String>,
    ) -> Result<Buffer> {
        let r = thread_local(&self.repo.inner);
        let oid =
            gix::hash::ObjectId::from_hex(id.as_bytes()).map_err(|e| err("parse_oid", e))?;
        let blob = r.find_blob(oid).map_err(|e| err("find_blob", e))?;
        self.convert_to_worktree(
            Buffer::from(blob.data.clone()),
            rela_path,
            can_delay,
            unknown_encoding,
        )
    }
}

impl FilterPipeline {
    fn convert_to_git_read<R: Read>(&mut self, src: R, rela_path: &str) -> Result<Vec<u8>> {
        let r = thread_local(&self.repo.inner);
        let index = r.index_or_empty().map_err(|e| err("index_or_empty", e))?;
        let mut cache = r
            .attributes_only(
                &index,
                gix::worktree::stack::state::attributes::Source::WorktreeThenIdMapping,
            )
            .map_err(|e| err("attributes_only", e))?;
        let entry = cache
            .at_path(rela_path, None)
            .map_err(|e| err("at_path", e))?;
        let outcome = self
            .pipeline
            .convert_to_git(
                src,
                std::path::Path::new(rela_path),
                &mut |_, out| {
                    entry.matching_attributes(out);
                },
                &mut |buf: &mut Vec<u8>| -> std::result::Result<Option<()>, gix::Error> {
                    let bstr =
                        gix::path::to_unix_separators_on_windows(gix::bstr::BStr::new(rela_path));
                    let e = match index.entry_by_path(bstr.as_ref()) {
                        None => return Ok(None),
                        Some(e) => e,
                    };
                    let obj = r.objects.try_find(&e.id, buf)?;
                    Ok(obj.filter(|o| o.kind == gix::objs::Kind::Blob).map(|_| ()))
                },
            )
            .map_err(|e| err("convert_to_git", e))?;
        drain_git(outcome)
    }
}

fn drain_git(outcome: ToGitOutcome<impl Read>) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    match outcome {
        ToGitOutcome::Unchanged(mut r) => {
            r.read_to_end(&mut buf).map_err(|e| err("read_to_end", e))?;
        }
        ToGitOutcome::Process(mut r) => {
            r.read_to_end(&mut buf).map_err(|e| err("read_to_end", e))?;
        }
        ToGitOutcome::Buffer(b) => {
            buf.extend_from_slice(b);
        }
    }
    Ok(buf)
}

fn drain_worktree(outcome: ToWorktreeOutcome) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    match outcome {
        ToWorktreeOutcome::Unchanged(b) => buf.extend_from_slice(b),
        ToWorktreeOutcome::Buffer(b) => buf.extend_from_slice(b),
        ToWorktreeOutcome::Process(m) => match m {
            MaybeDelayed::Immediate(mut r) => {
                r.read_to_end(&mut buf).map_err(|e| err("read_to_end", e))?;
            }
            MaybeDelayed::Delayed(_) => {
                return Err(err_str(
                    "convert_to_worktree",
                    "delayed processing is forbidden",
                ));
            }
        },
    }
    Ok(buf)
}

/// Extract pipeline options from `repo`'s configuration (core.safecrlf, core.autocrlf, core.eol,
/// core.checkRoundtripEncoding, and filter drivers). Leniency is treated as strict so that
/// malformed configuration surfaces as an error rather than being silently ignored.
fn build_options(r: &gix::Repository) -> Result<gix_filter::pipeline::Options> {
    let config = r.config_snapshot();
    let encodings = Core::CHECK_ROUND_TRIP_ENCODING
        .try_into_encodings(config.string("core.checkRoundtripEncoding"))
        .map_err(|e| err("core.checkRoundtripEncoding", e))?;
    let safe_crlf = config
        .string("core.safecrlf")
        .map(|value| Core::SAFE_CRLF.try_into_safecrlf(value))
        .transpose()
        .map_err(|e| err("core.safecrlf", e))?
        .unwrap_or(gix_filter::pipeline::CrlfRoundTripCheck::Fail);
    let auto_crlf = config
        .string("core.autocrlf")
        .map(|value| Core::AUTO_CRLF.try_into_autocrlf(value))
        .transpose()
        .map_err(|e| err("core.autocrlf", e))?
        .unwrap_or_default();
    let eol = config
        .string("core.eol")
        .map(|value| Core::EOL.try_into_eol(value))
        .transpose()
        .map_err(|e| err("core.eol", e))?;
    let drivers = extract_drivers(r)?;
    Ok(gix_filter::pipeline::Options {
        drivers,
        eol_config: gix_filter::eol::Configuration { auto_crlf, eol },
        crlf_roundtrip_check: safe_crlf,
        encodings_with_roundtrip_check: encodings,
    })
}

/// Collect the configured filter drivers from all `filter.*` sections.
fn extract_drivers(r: &gix::Repository) -> Result<Vec<gix_filter::Driver>> {
    let config = r.config_snapshot();
    let mut drivers: Vec<gix_filter::Driver> = Vec::new();
    for section in config.sections_by_name("filter").into_iter().flatten() {
        let Some(name) = section.header().subsection_name() else {
            continue;
        };
        let driver = match drivers.iter_mut().find(|d| d.name == name) {
            Some(driver) => driver,
            None => {
                drivers.push(gix_filter::Driver {
                    name: name.to_owned(),
                    clean: None,
                    smudge: None,
                    process: None,
                    required: false,
                });
                drivers.last_mut().expect("the driver was just added")
            }
        };
        if let Some(value) = section.value("clean") {
            driver.clean = Some(value.to_owned());
        }
        if let Some(value) = section.value("smudge") {
            driver.smudge = Some(value.to_owned());
        }
        if let Some(value) = section.value("process") {
            driver.process = Some(value.to_owned());
        }
        if let Some(value) = section.value("required") {
            driver.required = gix::config::Boolean::try_from(BStr::new(&value))
                .map_err(|e| err("filter_required", e))?
                .into();
        }
    }
    Ok(drivers)
}

/// The outcome of [`FilterPipeline::worktree_file_to_object`]: the object id, its entry kind, and
/// basic filesystem metadata of the source file.
#[napi(object)]
pub struct FilterFileObject {
    /// The hex object id written to the database.
    pub id: String,
    /// The entry kind, Debug-formatted (`Blob`, `BlobExecutable`, `Link`, …).
    pub kind: String,
    /// Whether the source file is executable.
    pub is_executable: bool,
    /// Whether the source file is a symlink.
    pub is_symlink: bool,
    /// Whether the source file is a regular file.
    pub is_file: bool,
    /// Whether the source path is a directory.
    pub is_dir: bool,
}

/// The outcome of [`FilterPipeline::shutdown`]: one entry per terminated filter process.
#[napi(object)]
pub struct FilterShutdownOutcome {
    /// Each process command and its exit code, or `null` if it was zombified (not waited on).
    pub processes: Vec<FilterProcessOutcome>,
}

/// A single terminated filter process.
#[napi(object)]
pub struct FilterProcessOutcome {
    /// The filter command that was invoked.
    pub command: String,
    /// The process exit code, or `null` if it was zombified (not waited on).
    pub exit_status: Option<i32>,
}

/// A configured filter driver (from a `filter.<name>.*` section).
#[napi(object)]
pub struct FilterDriver {
    /// The driver name (the `<name>` in `filter.<name>`).
    pub name: String,
    /// The `clean` command (worktree → git), if configured.
    pub clean: Option<String>,
    /// The `smudge` command (git → worktree), if configured.
    pub smudge: Option<String>,
    /// The long-running `process` command, if configured.
    pub process: Option<String>,
    /// Whether the driver is `required` (fail if the filter errors).
    pub required: bool,
}
