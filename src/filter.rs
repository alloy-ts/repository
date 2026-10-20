//! NAPI bindings for git's content-filter pipeline (smudge/clean, CRLF/EOL, attribute-driven
//! keyword expansion) that runs when moving bytes between the working tree and the object
//! database. This wraps [`gix::filter::Pipeline`], mirroring the primitive exposed by
//! `gix::Repository::filter_pipeline()` (which is not available in this gix version).
use std::io::Read;

use napi::Result;
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use gix::bstr::BStr;
use gix::objs::Find;

use crate::error::{bail, err, message, thread_local};
use crate::repository::Repository;

/// A configured filter pipeline bound to a repository.
///
/// Construct it with the owning [`Repository`]; each conversion rebuilds the attribute cache and
/// index lookup on the fly (cheap relative to the actual filtering work) and drains the resulting
/// stream into a [`Buffer`] for the FFI boundary.
#[napi]
pub struct FilterPipeline {
    repo: Repository,
}

#[napi]
impl FilterPipeline {
    /// Create a pipeline bound to `repo`.
    #[napi(constructor)]
    pub fn new(repo: &Repository) -> Self {
        FilterPipeline {
            repo: Repository {
                inner: repo.inner.clone(),
            },
        }
    }

    /// Convert `src` (located at repo-relative `rela_path`) from the working-tree representation
    /// to the representation stored in `git`. Returns the (possibly filtered) bytes.
    #[napi]
    pub fn convert_to_git(&mut self, src: Buffer, rela_path: String) -> Result<Buffer> {
        self.convert_to_git_read(src.as_ref(), &rela_path)
            .map(Buffer::from)
    }

    /// Convert `src` (located at repo-relative `rela_path`) from what is stored in `git` to the
    /// working-tree representation. Delayed/process filters are disabled (`can_delay: false`), so
    /// the full result is always available synchronously. Returns the (possibly filtered) bytes.
    #[napi]
    pub fn convert_to_worktree(&mut self, src: Buffer, rela_path: String) -> Result<Buffer> {
        let r = thread_local(&self.repo.inner);
        let options = build_options(&r)?;
        let context = r.command_context()?;
        let mut pipeline = gix::filter::Pipeline::new(context, r.object_hash(), options);
        let index = r.index_or_empty()?;
        let mut cache = r.attributes_only(
            &index,
            gix::worktree::stack::state::attributes::Source::WorktreeThenIdMapping,
        )?;
        let entry = cache.at_path(rela_path.as_str(), None)?;
        let outcome = pipeline
            .convert_to_worktree(
                src.as_ref(),
                gix::path::into_bstr(rela_path.as_str())?.as_ref(),
                &mut |_, out| {
                    entry.matching_attributes(out);
                },
                gix::filter::pipeline::convert::to_worktree::Options {
                    can_delay: false,
                    unknown_encoding:
                        gix::filter::pipeline::convert::to_worktree::UnknownEncoding::Ignore,
                },
            )
            .map_err(|e| err("convert_to_worktree", e))?;
        drain_worktree(outcome).map(Buffer::from)
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
            err(
                "worktree_file_to_object",
                "Cannot add worktree files in bare repositories",
            )
        })?;
        let path = worktree_dir.join(gix::path::from_bstr(rela_path.as_str())?);
        let md = match std::fs::symlink_metadata(&path) {
            Ok(md) => md,
            Err(err) => {
                if gix::fs::io_err::is_not_found(err.kind(), err.raw_os_error()) {
                    return Ok(None);
                } else {
                    bail!(err.and_raise(message!(
                        "Failed to perform IO for object creation for \"{}\"",
                        path.display()
                    )));
                }
            }
        };
        let (id, kind) = if md.is_symlink() {
            let target = std::fs::read_link(&path)?;
            let id = r.write_blob(gix::path::into_bstr(target)?)?;
            (id, gix::objs::tree::EntryKind::Link)
        } else if md.is_file() {
            let file = std::fs::File::open(&path)?;
            let bytes = self.convert_to_git_read(file, &rela_path)?;
            let id = r.write_blob(&bytes)?;
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
}

impl FilterPipeline {
    fn convert_to_git_read<R: Read>(&self, src: R, rela_path: &str) -> Result<Vec<u8>> {
        let r = thread_local(&self.repo.inner);
        let options = build_options(&r)?;
        let context = r.command_context()?;
        let mut pipeline = gix::filter::Pipeline::new(context, r.object_hash(), options);
        let index = r.index_or_empty()?;
        let mut cache = r.attributes_only(
            &index,
            gix::worktree::stack::state::attributes::Source::WorktreeThenIdMapping,
        )?;
        let entry = cache.at_path(rela_path, None)?;
        let outcome = pipeline
            .convert_to_git(
                src,
                std::path::Path::new(rela_path),
                &mut |_, out| {
                    entry.matching_attributes(out);
                },
                &mut |buf: &mut Vec<u8>| -> Result<Option<()>> {
                    let bstr = gix::path::to_unix_separators_on_windows(gix::path::into_bstr(
                        std::path::Path::new(rela_path),
                    )?);
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

fn drain_git(outcome: gix::filter::pipeline::convert::ToGitOutcome<impl Read>) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    match outcome {
        gix::filter::pipeline::convert::ToGitOutcome::Unchanged(mut r) => {
            r.read_to_end(&mut buf)?;
        }
        gix::filter::pipeline::convert::ToGitOutcome::Process(mut r) => {
            r.read_to_end(&mut buf)?;
        }
        gix::filter::pipeline::convert::ToGitOutcome::Buffer(b) => {
            buf.extend_from_slice(b);
        }
    }
    Ok(buf)
}

fn drain_worktree(outcome: gix::filter::pipeline::convert::ToWorktreeOutcome) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    match outcome {
        gix::filter::pipeline::convert::ToWorktreeOutcome::Unchanged(b) => buf.extend_from_slice(b),
        gix::filter::pipeline::convert::ToWorktreeOutcome::Buffer(b) => buf.extend_from_slice(b),
        gix::filter::pipeline::convert::ToWorktreeOutcome::Process(mut m) => {
            m.read_to_end(&mut buf)?;
        }
    }
    Ok(buf)
}

/// Extract pipeline options from `repo`'s configuration (core.safecrlf, core.autocrlf, core.eol,
/// core.checkRoundtripEncoding, and filter drivers), honoring the repository's leniency setting.
fn build_options(r: &gix::Repository) -> Result<gix::filter::pipeline::Options> {
    use crate::config::cache::util::{ApplyLeniency, ApplyLeniencyDefaultValue};
    use crate::config::tree::Core;

    let lenient = r.config.lenient_config;
    let config = &r.config.resolved;
    let encodings = Core::CHECK_ROUND_TRIP_ENCODING
        .try_into_encodings(config.string("core.checkRoundtripEncoding"))?;
    let safe_crlf = config
        .string("core.safecrlf")
        .map(|value| Core::SAFE_CRLF.try_into_safecrlf(value))
        .transpose()
        .map(Option::unwrap_or_default)
        .with_lenient_default_value(lenient, gix::filter::pipeline::CrlfRoundTripCheck::Fail)?;
    let auto_crlf = config
        .string("core.autocrlf")
        .map(|value| Core::AUTO_CRLF.try_into_autocrlf(value))
        .transpose()
        .with_leniency(lenient)?
        .unwrap_or_default();
    let eol = config
        .string("core.eol")
        .map(|value| Core::EOL.try_into_eol(value))
        .transpose()?;
    let drivers = extract_drivers(r)?;
    Ok(gix::filter::pipeline::Options {
        drivers,
        eol_config: gix::filter::eol::Configuration { auto_crlf, eol },
        encodings_with_roundtrip_check: encodings,
        crlf_roundtrip_check: safe_crlf,
    })
}

/// Collect the configured filter drivers, ignoring those in sections we don't trust enough.
fn extract_drivers(r: &gix::Repository) -> Result<Vec<gix::filter::Driver>> {
    let mut drivers = Vec::new();
    for section in r
        .config
        .resolved
        .sections_by_name("filter")
        .into_iter()
        .flatten()
        .filter(|section| (r.filter_config_section())(section.meta()))
    {
        let Some(name) = section.header().subsection_name() else {
            continue;
        };
        let driver = match drivers.iter_mut().find(|d| d.name == name) {
            Some(driver) => driver,
            None => {
                drivers.push(gix::filter::Driver {
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
            driver.required = gix::config::Boolean::try_from(BStr::new(value))
                .or_raise(|| {
                    message!("Could not interpret 'filter.{name}.required' configuration")
                })?
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
