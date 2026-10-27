use std::sync::Arc;

use crate::error::RepositoryResult as Result;
use gix::bstr::BStr;
use gix::config::tree::{Gpg, gpg};
use gix::objs::signature::verify::TrustLevel;
use gix::objs::signature::{self, Format};
use gix::refs::transaction::{LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{Category, Target, TargetRef};
use napi::bindgen_prelude::Buffer;
use napi::bindgen_prelude::Either;
use napi_derive::napi;

use crate::blob::{Blob, build_blob};
use crate::commit::{Commit, VerifyOutcome, build_commit, signing_options};
use crate::error::{err, err_str, parse_oid, thread_local};
use crate::identity::{Signature, signature_from_dto, to_signature};
use crate::object::{Object, build_object, kind_to_string};
use crate::reference::{Reference, build_reference};
use crate::repository::Repository;
use crate::tree::{Tree, build_tree};

/// Whether a tag carries its own object (annotated) or is a plain ref (lightweight).
#[napi(string_enum)]
pub enum TagKind {
    /// A tag object with its own metadata (message, tagger, date).
    Annotated,
    /// A plain ref pointing directly at an object (usually a commit).
    Lightweight,
}

#[napi]
pub struct Tag {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) id: gix::ObjectId,
}

#[napi]
impl Tag {
    #[napi(getter)]
    pub fn name(&self) -> napi::Result<String> {
        let r = thread_local(&self.repo);
        let tag = r.find_tag(self.id).map_err(|e| err("find_tag", e))?;
        let tag = tag.decode().map_err(|e| err("decode", e))?;
        Ok(String::from_utf8_lossy(&tag.name).into_owned())
    }

    #[napi(getter)]
    pub fn message(&self) -> napi::Result<String> {
        let r = thread_local(&self.repo);
        let tag = r.find_tag(self.id).map_err(|e| err("find_tag", e))?;
        let tag = tag.decode().map_err(|e| err("decode", e))?;
        Ok(String::from_utf8_lossy(&tag.message).into_owned())
    }

    /// The object id of this tag.
    #[napi(getter)]
    pub fn id(&self) -> String {
        self.id.to_string()
    }

    /// The id of the object this tag points to.
    #[napi(getter)]
    pub fn target_id(&self) -> napi::Result<String> {
        let r = thread_local(&self.repo);
        let tag = r.find_tag(self.id).map_err(|e| err("find_tag", e))?;
        let tag = tag.decode().map_err(|e| err("decode", e))?;
        Ok(tag.target.to_string())
    }

    /// The kind of the object this tag points to (`commit`, `tree`, `blob`, or `tag`).
    #[napi(getter)]
    pub fn target_kind(&self) -> napi::Result<String> {
        let r = thread_local(&self.repo);
        let tag = r.find_tag(self.id).map_err(|e| err("find_tag", e))?;
        let tag = tag.decode().map_err(|e| err("decode", e))?;
        Ok(kind_to_string(tag.target_kind))
    }

    /// The in-body cryptographic signature (armored, including its `-----BEGIN ... SIGNATURE-----`
    /// block) if the tag is signed, otherwise `null`. Presence only means the tag *carries* a
    /// signature; it does not prove the signature is valid (see [`Tag::verify`]).
    #[napi(getter)]
    pub fn signature(&self) -> napi::Result<Option<String>> {
        let r = thread_local(&self.repo);
        let tag = r.find_tag(self.id).map_err(|e| err("find_tag", e))?;
        let tag = tag.decode().map_err(|e| err("decode", e))?;
        Ok(tag
            .signature
            .map(|s| String::from_utf8_lossy(&s).into_owned()))
    }

    /// The object this tag points at.
    ///
    /// For a release tag this is normally the tagged commit, but a tag may point at any
    /// object (`tree`, `blob`, or another `tag`). Use [`Tag::peel_to_commit`] & friends to
    /// follow tag chains down to a specific kind.
    #[napi]
    pub fn target(&self) -> napi::Result<Object> {
        let r = thread_local(&self.repo);
        let tag = r.find_tag(self.id).map_err(|e| err("find_tag", e))?;
        let target = tag.target_id().map_err(|e| err("target_id", e))?;
        build_object(&self.repo, target.detach())
    }

    /// Follow tag chains and peel this tag to a commit. Throws when the tag cannot be
    /// peeled to a commit (e.g. a tag pointing at a blob).
    #[napi]
    pub fn peel_to_commit(&self) -> napi::Result<Commit> {
        let r = thread_local(&self.repo);
        let id = peeled_id(&self.repo, self.id, gix::objs::Kind::Commit)?;
        build_commit(
            &self.repo,
            r.find_commit(id).map_err(|e| err("find_commit", e))?,
        )
    }

    /// Follow tag chains and peel this tag to a tree.
    #[napi]
    pub fn peel_to_tree(&self) -> napi::Result<Tree> {
        let id = peeled_id(&self.repo, self.id, gix::objs::Kind::Tree)?;
        build_tree(&self.repo, id)
    }

    /// Follow tag chains and peel this tag to a blob.
    #[napi]
    pub fn peel_to_blob(&self) -> napi::Result<Blob> {
        let id = peeled_id(&self.repo, self.id, gix::objs::Kind::Blob)?;
        Ok(build_blob(&self.repo, id))
    }

    /// Follow tag chains and peel this tag to an object of the given kind
    /// (`commit`, `tree`, `blob`, or `tag`).
    #[napi]
    pub fn peel_to_kind(&self, kind: String) -> napi::Result<Object> {
        let kind = parse_kind("peel_to_kind", &kind)?;
        build_object(&self.repo, peeled_id(&self.repo, self.id, kind)?)
    }

    /// Follow a tag-of-tag chain by one hop.
    ///
    /// If this tag's target is itself a tag object, returns that next [`Tag`]; otherwise throws
    /// (e.g. a tag pointing directly at a commit has no further tag to peel to).
    #[napi]
    pub fn peel_to_tag(&self) -> napi::Result<Tag> {
        let r = thread_local(&self.repo);
        let tag = r.find_tag(self.id).map_err(|e| err("find_tag", e))?;
        let decoded = tag.decode().map_err(|e| err("decode", e))?;
        if decoded.target_kind != gix::objs::Kind::Tag {
            return Err(err_str("peel_to_tag", "tag target is not a tag object"));
        }
        let next = r
            .find_tag(gix::ObjectId::from_hex(&decoded.target).map_err(|e| err("oid", e))?)
            .map_err(|e| err("find_tag", e))?;
        build_tag(&self.repo, next)
    }

    /// The raw, decompressed body of this tag object (without the loose-object header).
    #[napi]
    pub fn data(&self) -> napi::Result<Buffer> {
        let r = thread_local(&self.repo);
        let obj = r.find_object(self.id).map_err(|e| err("find_object", e))?;
        Ok(Buffer::from(obj.detach().data))
    }

    /// The tagger identity, if this is an annotated tag.
    #[napi]
    pub fn tagger(&self) -> napi::Result<Option<Signature>> {
        let r = thread_local(&self.repo);
        let tag = r.find_tag(self.id).map_err(|e| err("find_tag", e))?;
        let tag = tag.decode().map_err(|e| err("decode", e))?;
        let tagger = tag.tagger().map_err(|e| err("tagger", e))?;
        match tagger {
            Some(s) => {
                let time = s.time().map_err(|e| err("tagger.time", e))?;
                Ok(Some(to_signature(
                    s.name,
                    s.email,
                    time.seconds,
                    time.offset,
                )))
            }
            None => Ok(None),
        }
    }

    /// Sign this annotated tag, producing a *new* tag object that carries the signature, and return it.
    ///
    /// The signing key and program are taken from the repository configuration
    /// (`user.signingKey`, `gpg.format`, `gpg.<format>.program`, etc.), mirroring `git tag -s`.
    #[napi]
    pub fn sign(&self) -> napi::Result<Tag> {
        let r = thread_local(&self.repo);
        let id = sign_tag_object(&r, self.id)?;
        Ok(Tag {
            repo: Arc::clone(&self.repo),
            id,
        })
    }

    /// Verify this tag's signature.
    ///
    /// Returns `null` if the tag is not signed. Otherwise returns the verification outcome
    /// (`format`, `status`, `trustLevel`, `signer`, `key`, `fingerprint`, `output`, `valid`, ...).
    /// This shells out to the configured GPG/SSH/X.509 program, so it can be slow.
    #[napi]
    pub fn verify(&self) -> napi::Result<Option<VerifyOutcome>> {
        let r = thread_local(&self.repo);
        let obj = r.find_object(self.id).map_err(|e| err("find_object", e))?;
        let Some((signature, signed_data)) = gix::objs::TagRefIter::signature(&obj.data) else {
            return Ok(None);
        };

        let config = r.config_snapshot();
        let minimum_trust = config
            .string(Gpg::MIN_TRUST_LEVEL)
            .map(|v| Gpg::MIN_TRUST_LEVEL.try_into_trust_level(v))
            .transpose()
            .map_err(|e| err("gpg.minTrustLevel", e))?
            .unwrap_or(TrustLevel::Undefined);

        let format = signature.format;
        let program = crate::commit::signature_program(&config, format)?;
        let options = match format {
            Format::OpenPgp => signature::verify::Options::OpenPgp {
                program,
                program_arguments: Vec::new(),
                environment: Vec::new(),
                minimum_trust,
            },
            Format::X509 => signature::verify::Options::X509 {
                program,
                program_arguments: Vec::new(),
                environment: Vec::new(),
                minimum_trust,
            },
            Format::Ssh => {
                let allowed_signers = config
                    .trusted_path(gpg::Ssh::ALLOWED_SIGNERS_FILE)
                    .map_err(|e| err("gpg.ssh.allowedSignersFile", e))?
                    .ok_or_else(|| {
                        err_str(
                            "verify",
                            "gpg.ssh.allowedSignersFile must be configured for SSH signature verification",
                        )
                    })?;
                let revocation_file = config
                    .trusted_path(gpg::Ssh::REVOCATION_FILE)
                    .map_err(|e| err("gpg.ssh.revocationFile", e))?
                    .filter(|p| p.exists());
                let verify_time = match obj.to_tag_ref().tagger().map_err(|e| err("tagger", e))? {
                    Some(tagger) => tagger.time().map_err(|e| err("tagger.time", e))?,
                    None => {
                        let seconds = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map_err(|e| err("verify_time", e))?
                            .as_secs() as i64;
                        gix::date::Time { seconds, offset: 0 }
                    }
                };
                signature::verify::Options::Ssh {
                    program,
                    program_arguments: Vec::new(),
                    environment: Vec::new(),
                    allowed_signers: crate::commit::resolve_relative_to_repository(
                        &r,
                        allowed_signers,
                    ),
                    revocation_file: revocation_file
                        .map(|p| crate::commit::resolve_relative_to_repository(&r, p)),
                    verify_time,
                    minimum_trust,
                }
            }
        };

        let outcome = signed_data
            .verify(signature.data, options)
            .map_err(|e| err("verify", e))?;
        Ok(Some(VerifyOutcome::from(outcome)))
    }
}

/// Construct a [`Tag`] wrapper from a repository and a decoded tag (used by `Repository` / `GitObject`).
pub fn build_tag(repo: &Arc<gix::ThreadSafeRepository>, tag: gix::Tag) -> napi::Result<Tag> {
    Ok(Tag {
        repo: Arc::clone(repo),
        id: tag.id,
    })
}

/// Peel `id` to `kind`, following tag (and commit) chains, and return the final object id.
fn peeled_id(
    repo: &Arc<gix::ThreadSafeRepository>,
    id: gix::ObjectId,
    kind: gix::objs::Kind,
) -> napi::Result<gix::ObjectId> {
    let r = thread_local(repo);
    let obj = r.find_object(id).map_err(|e| err("find_object", e))?;
    let peeled = obj.peel_to_kind(kind).map_err(|e| err("peel", e))?;
    Ok(peeled.id)
}

/// Parse an object-kind name (`commit`, `tree`, `blob`, `tag`), case-insensitively.
fn parse_kind(what: &str, kind: &str) -> napi::Result<gix::objs::Kind> {
    match kind.to_lowercase().as_str() {
        "commit" => Ok(gix::objs::Kind::Commit),
        "tree" => Ok(gix::objs::Kind::Tree),
        "blob" => Ok(gix::objs::Kind::Blob),
        "tag" => Ok(gix::objs::Kind::Tag),
        other => Err(err_str(what, format!("invalid target_kind {other}"))),
    }
}

/// Resolve the kind of a tag target: explicitly given, else looked up from the object.
fn resolve_target_kind(
    what: &str,
    kind: Option<&str>,
    r: &gix::Repository,
    target: gix::ObjectId,
) -> napi::Result<gix::objs::Kind> {
    match kind {
        Some(kind) => parse_kind(what, kind),
        None => r
            .find_object(target)
            .map(|o| o.kind)
            .map_err(|e| err("find_object", e)),
    }
}

/// Sign the tag object `id` with the repository's configured signing key and program
/// (`user.signingKey`, `gpg.format`, ...), writing the signed object and returning its id.
///
/// The original object is left untouched: signing produces a *new* object, so any
/// `refs/tags/<name>` reference still points at the unsigned one.
fn sign_tag_object(r: &gix::Repository, id: gix::ObjectId) -> napi::Result<gix::ObjectId> {
    let options = signing_options(r)?;
    let signed = r
        .find_tag(id)
        .map_err(|e| err("find_tag", e))?
        .decode()
        .map_err(|e| err("decode", e))?
        .sign(options)
        .map_err(|e| err("sign", e))?;
    r.write_object(&signed)
        .map_err(|e| err("write_object", e))
        .map(|oid| oid.detach())
}

/// Convert a full (`refs/tags/v1`) or short (`v1`) tag name into a `FullName`.
///
/// Names that already start with `refs/` are used as-is and must belong to the tag
/// category, so `refs/heads/main` is rejected instead of becoming `refs/tags/refs/heads/main`.
pub(crate) fn to_tag_full_name(name: &str) -> napi::Result<gix::refs::FullName> {
    let full = if name.starts_with("refs/") {
        gix::refs::FullName::try_from(name).map_err(|e| err("ref_name", e))?
    } else {
        Category::Tag
            .to_full_name(BStr::new(name.as_bytes()))
            .map_err(|e| err("ref_name", e))?
    };
    if full.category() != Some(Category::Tag) {
        return Err(err_str("tag_name", format!("'{name}' is not a tag")));
    }
    Ok(full)
}

/// The short name of a tag reference (`v1` for `refs/tags/v1`).
fn short_tag_name(full: &gix::refs::FullName) -> String {
    String::from_utf8_lossy(full.shorten()).into_owned()
}

/// Whether `name` is a valid tag name. Accepts short names (`v1`); unlike Git, names
/// already carrying the `refs/tags/` prefix are accepted as well.
#[napi]
pub fn tag_name_is_valid(name: String) -> bool {
    Category::Tag
        .to_full_name(BStr::new(name.as_bytes()))
        .is_ok()
}

/// A tag as returned by [`Repository::list_tags`] — richer than a bare `Reference` and
/// far cheaper than issuing one `findTagByName` per tag.
#[napi(object)]
pub struct TagInfo {
    /// Short tag name (e.g. `"v1.2.3"`).
    pub name: String,
    /// Annotated (carries a tag object) or lightweight (a plain ref).
    pub kind: TagKind,
    /// Id of the object the tag points at (the tag's immediate target).
    pub target: String,
    /// Kind of `target` (`commit`, `tree`, `blob`, or `tag`).
    pub target_kind: String,
    /// Subject/message — `null` for lightweight tags.
    pub message: Option<String>,
    /// Tagger identity — `null` for lightweight tags.
    pub tagger: Option<Signature>,
}

/// Input for writing an annotated tag object.
#[derive(Clone)]
#[napi(object)]
pub struct TagInput {
    pub name: String,
    pub target: String,
    /// The kind of `target` (`commit`, `tree`, `blob`, or `tag`); inferred by looking the
    /// object up when omitted.
    pub target_kind: Option<String>,
    pub message: String,
    pub tagger: Option<Signature>,
    /// Optional pre-computed in-body signature (armored) to embed in the tag object.
    pub signature: Option<String>,
    /// Sign the tag object with the repository's configured key and program before writing
    /// it, mirroring `git tag -s`.
    pub sign: Option<bool>,
}

/// Options controlling [`Repository::create_tag`].
#[derive(Clone, Default)]
#[napi(object)]
pub struct CreateTagOptions {
    /// The tag message. Setting it (or `annotated`, or `sign`) creates an *annotated* tag
    /// object; without it a lightweight tag reference is created.
    pub message: Option<String>,
    /// Create an annotated tag object even with an empty message.
    pub annotated: Option<bool>,
    /// The tagger identity; defaults to the configured committer.
    pub tagger: Option<Signature>,
    /// Sign the annotated tag object with the configured key and program (`git tag -s`).
    /// Implies `annotated`.
    pub sign: Option<bool>,
    /// The kind of `target` (`commit`, `tree`, `blob`, or `tag`); inferred from the object
    /// when omitted.
    pub target_kind: Option<String>,
    /// Overwrite an existing tag of the same name instead of failing.
    pub force: Option<bool>,
}

#[napi]
impl Repository {
    /// Find and decode a tag by id.
    #[napi]
    pub fn find_tag(&self, id: String) -> napi::Result<Tag> {
        let r = thread_local(&self.inner);
        let tag = r
            .find_tag(parse_oid(&id)?)
            .map_err(|e| err("find_tag", e))?;
        build_tag(&self.inner, tag)
    }

    /// Write an annotated tag object directly to the object database.
    ///
    /// This only writes the object — no `refs/tags/<name>` reference is created. Returns its id.
    #[napi]
    pub fn write_tag(&self, input: TagInput) -> napi::Result<String> {
        let r = thread_local(&self.inner);
        let target = parse_oid(&input.target)?;
        let target_kind =
            resolve_target_kind("write_tag", input.target_kind.as_deref(), &r, target)?;

        let default_tagger = r.committer().and_then(|res| res.ok()).map(|s| s.into());
        let tagger = match input.tagger {
            Some(s) => Some(signature_from_dto(&s)?),
            None => default_tagger,
        };

        let tag = gix::objs::Tag {
            target,
            target_kind,
            name: input.name.into(),
            tagger,
            message: input.message.into(),
            signature: input.signature.map(Into::into),
        };

        let mut oid: gix::ObjectId = r
            .write_object(&tag)
            .map_err(|e| err("write_tag", e))?
            .detach();
        if input.sign.unwrap_or(false) {
            oid = sign_tag_object(&r, oid)?;
        }
        Ok(oid.to_string())
    }

    /// All tags (`refs/tags/*`) of the repository, mirroring [`localBranches`](Self::local_branches).
    ///
    /// The returned references may point at a tag object (annotated tag) or directly at any
    /// object (lightweight tag); use [`Reference::peel_to_tag`] to reach the tag object.
    #[napi]
    pub fn tags(&self) -> napi::Result<Vec<Reference>> {
        let r = thread_local(&self.inner);
        let platform = r.references().map_err(|e| err("references", e))?;
        let iter = platform.tags().map_err(|e| err("tags", e))?;
        let mut out = Vec::new();
        for reference in iter {
            let reference = reference.map_err(|e| err("tag", e))?;
            out.push(build_reference(&self.inner, reference)?);
        }
        Ok(out)
    }

    /// The names of all tags, shortened (`v1.0.0` for `refs/tags/v1.0.0`) and sorted.
    ///
    /// Cheaper than [`tags`](Self::tags) because the references are not materialised into
    /// objects.
    #[napi]
    pub fn tag_names(&self) -> napi::Result<Vec<String>> {
        let r = thread_local(&self.inner);
        let platform = r.references().map_err(|e| err("references", e))?;
        let iter = platform.tags().map_err(|e| err("tags", e))?;
        let mut names = Vec::new();
        for reference in iter {
            let reference = reference.map_err(|e| err("tag", e))?;
            names.push(String::from_utf8_lossy(reference.name().shorten()).into_owned());
        }
        names.sort();
        Ok(names)
    }

    /// All tags (`refs/tags/*`) with kind, target, message and tagger filled in — richer than
    /// [`tags`](Self::tags) (bare references) and far cheaper than one `findTagByName` per tag.
    ///
    /// When `pattern` is set, only tags whose short name (`v1.0.0`) or full ref name
    /// (`refs/tags/v1.0.0`) starts with it are returned.
    #[napi]
    pub fn list_tags(&self, pattern: Option<String>) -> napi::Result<Vec<TagInfo>> {
        let r = thread_local(&self.inner);
        let platform = r.references().map_err(|e| err("references", e))?;
        let iter = platform.tags().map_err(|e| err("tags", e))?;
        let mut out = Vec::new();
        for reference in iter {
            let reference = reference.map_err(|e| err("tag", e))?;
            let short = String::from_utf8_lossy(reference.name().shorten()).into_owned();
            if let Some(pat) = &pattern {
                let full = reference.name().to_string();
                if !short.starts_with(pat.as_str()) && !full.starts_with(pat.as_str()) {
                    continue;
                }
            }
            let mut reference = reference;
            let (kind, target, target_kind, message, tagger) = match reference.peel_to_tag() {
                Ok(tag) => {
                    let tag = tag.decode().map_err(|e| err("decode", e))?;
                    let message = if tag.message.is_empty() {
                        None
                    } else {
                        Some(String::from_utf8_lossy(&tag.message).into_owned())
                    };
                    let tagger = match tag.tagger().map_err(|e| err("tagger", e))? {
                        Some(s) => {
                            let time = s.time().map_err(|e| err("tagger.time", e))?;
                            Some(to_signature(s.name, s.email, time.seconds, time.offset))
                        }
                        None => None,
                    };
                    (
                        TagKind::Annotated,
                        tag.target.to_string(),
                        kind_to_string(tag.target_kind),
                        message,
                        tagger,
                    )
                }
                Err(_) => {
                    // Lightweight: the reference points directly at an object.
                    let id = reference
                        .peel_to_id()
                        .map_err(|e| err("peel_to_id", e))?
                        .detach();
                    let kind = {
                        let obj = r.find_object(id).map_err(|e| err("find_object", e))?;
                        kind_to_string(obj.kind)
                    };
                    (TagKind::Lightweight, id.to_string(), kind, None, None)
                }
            };
            out.push(TagInfo {
                name: short,
                kind,
                target,
                target_kind,
                message,
                tagger,
            });
        }
        Ok(out)
    }

    /// Find an annotated tag by its short (`v1.0.0`) or full (`refs/tags/v1.0.0`) name and
    /// return its tag object.
    ///
    /// Throws when the tag does not exist, and when it is *lightweight* — a reference that
    /// points straight at an object with no tag object in between. Use
    /// [`find_reference`](Self::find_reference) for those, or
    /// [`create_tag`](Self::create_tag) with a `message` to make it annotated.
    #[napi]
    pub fn find_tag_by_name(&self, name: String) -> napi::Result<Tag> {
        let r = thread_local(&self.inner);
        let full = to_tag_full_name(&name)?;
        let mut reference = r
            .find_reference(full.to_string().as_str())
            .map_err(|e| err("find_tag_by_name", e))?;
        let tag = reference
            .peel_to_tag()
            .map_err(|e| err("find_tag_by_name", e))?;
        build_tag(&self.inner, tag)
    }

    /// Like [`find_tag_by_name`](Self::find_tag_by_name), but returns `null` instead of
    /// throwing when no tag of that name exists.
    #[napi]
    pub fn try_find_tag_by_name(&self, name: String) -> napi::Result<Option<Tag>> {
        let r = thread_local(&self.inner);
        let full = to_tag_full_name(&name)?;
        let Some(mut reference) = r
            .try_find_reference(full.to_string().as_str())
            .map_err(|e| err("try_find_tag_by_name", e))?
        else {
            return Ok(None);
        };
        let tag = reference
            .peel_to_tag()
            .map_err(|e| err("try_find_tag_by_name", e))?;
        Ok(Some(build_tag(&self.inner, tag)?))
    }

    /// Create the tag `name` (short `v1.0.0` or full `refs/tags/v1.0.0`) pointing at
    /// `target` — a 40-hex object id or a revision such as `HEAD` — and return its reference.
    ///
    /// Without a `message` this creates a *lightweight* tag (a reference pointing directly at
    /// `target`); with one — or with `annotated: true` — an *annotated* tag object is written
    /// and the reference points at that object. `tagger` overrides the configured committer,
    /// and `sign: true` signs the tag object with the configured key, mirroring `git tag -s`.
    /// Throws when the tag already exists unless `force` is set.
    ///
    /// The third argument accepts either the legacy message string
    /// (`createTag(name, target, "message", force?)`) or a [`CreateTagOptions`] object
    /// (`createTag(name, target, { message, annotated, tagger, sign, targetKind, force })`);
    /// omit it to create a lightweight tag. The legacy `force` argument only applies to the
    /// message form; in the options form `force` is part of the options object.
    #[napi]
    pub fn create_tag(
        &self,
        name: String,
        target: String,
        message_or_options: Option<Either<String, CreateTagOptions>>,
        force: Option<bool>,
    ) -> napi::Result<Reference> {
        let mut options = match message_or_options {
            None => CreateTagOptions::default(),
            Some(Either::A(message)) => CreateTagOptions {
                message: Some(message),
                force,
                ..Default::default()
            },
            Some(Either::B(mut options)) => {
                if options.force.is_none() {
                    options.force = force;
                }
                options
            }
        };
        let r = thread_local(&self.inner);
        let full = to_tag_full_name(&name)?;
        let short = short_tag_name(&full);
        let target_id = match parse_oid(&target) {
            Ok(id) => id,
            Err(_) => r
                .rev_parse_single(target.as_str())
                .map_err(|e| err("rev_parse_single", e))?
                .into(),
        };
        let annotated = options.annotated.unwrap_or(false)
            || options.sign.unwrap_or(false)
            || options.message.is_some();

        let tag_id = if annotated {
            let target_kind =
                resolve_target_kind("create_tag", options.target_kind.as_deref(), &r, target_id)?;
            let tagger = match options.tagger.as_ref() {
                Some(s) => Some(signature_from_dto(s)?),
                None => r.committer().and_then(|res| res.ok()).map(|s| s.into()),
            };
            let tag = gix::objs::Tag {
                target: target_id,
                target_kind,
                name: short.clone().into(),
                tagger,
                message: options.message.clone().unwrap_or_default().into(),
                signature: None,
            };
            let mut oid: gix::ObjectId = r
                .write_object(&tag)
                .map_err(|e| err("write_object", e))?
                .detach();
            if options.sign.unwrap_or(false) {
                oid = sign_tag_object(&r, oid)?;
            }
            oid
        } else {
            target_id
        };

        let constraint = if options.force.unwrap_or(false) {
            PreviousValue::Any
        } else {
            PreviousValue::MustNotExist
        };
        let reference = r
            .reference(
                full.to_string(),
                tag_id,
                constraint,
                format!("tag: created {short}"),
            )
            .map_err(|e| err("create_tag", e))?;
        build_reference(&self.inner, reference)
    }

    /// Delete the given tags (short or full names) and return the full ref names of the
    /// tags actually deleted, mirroring [`deleteLocalBranches`](Self::delete_local_branches).
    #[napi]
    pub fn delete_tags(&self, names: Vec<String>) -> napi::Result<Vec<String>> {
        let r = thread_local(&self.inner);
        let full_names = names
            .iter()
            .map(|n| to_tag_full_name(n))
            .collect::<Result<Vec<_>>>()?;
        let edits = full_names
            .iter()
            .map(|n| {
                RefEdit::delete_with_log(n.clone(), PreviousValue::MustExist, RefLog::AndReference)
            })
            .collect::<Vec<_>>();
        let deleted = r
            .edit_references(edits)
            .map_err(|e| err("delete_tags", e))?;
        Ok(deleted.into_iter().map(|e| e.name.to_string()).collect())
    }

    /// Rename the tag `oldName` to `newName` (short or full names) and return the new
    /// reference. Throws when the source does not exist or, without `force`, when the
    /// destination already exists.
    ///
    /// Only the reference is renamed: an annotated tag object keeps the name it was created
    /// with, so the tag's `name` still reads as the old one. Recreate the tag instead if the
    /// embedded name matters.
    #[napi]
    pub fn rename_tag(
        &self,
        old_name: String,
        new_name: String,
        force: Option<bool>,
    ) -> napi::Result<Reference> {
        let r = thread_local(&self.inner);
        let old_full = to_tag_full_name(&old_name)?;
        let new_full = to_tag_full_name(&new_name)?;
        let old_short = short_tag_name(&old_full);
        let new_short = short_tag_name(&new_full);

        let target = {
            let source = r
                .find_reference(old_full.to_string().as_str())
                .map_err(|e| err("rename_tag", e))?;
            match source.target() {
                TargetRef::Object(id) => Target::Object(id.to_owned()),
                TargetRef::Symbolic(name) => Target::Symbolic(name.to_owned()),
            }
        };
        if !force.unwrap_or(false)
            && r.try_find_reference(new_full.to_string().as_str())
                .map_err(|e| err("rename_tag", e))?
                .is_some()
        {
            return Err(err_str(
                "rename_tag",
                format!("tag '{new_name}' already exists"),
            ));
        }
        let previous = if force.unwrap_or(false) {
            PreviousValue::Any
        } else {
            PreviousValue::MustNotExist
        };
        let log = LogChange {
            mode: RefLog::AndReference,
            force_create_reflog: false,
            message: format!("tag: renamed {old_short} to {new_short}").into(),
        };
        let edits = vec![
            RefEdit::update_with_log(new_full.clone(), target, previous, log),
            RefEdit::delete_with_log(old_full, PreviousValue::MustExist, RefLog::AndReference),
        ];
        r.edit_references(edits).map_err(|e| err("rename_tag", e))?;
        build_reference(
            &self.inner,
            r.find_reference(new_full.to_string().as_str())
                .map_err(|e| err("rename_tag", e))?,
        )
    }
}
