use std::sync::Arc;

use gix::config::tree::{Gpg, gpg};
use gix::objs::signature::verify::TrustLevel;
use gix::objs::signature::{self, Format};
use napi::Result;
use napi_derive::napi;

use crate::commit::VerifyOutcome;
use crate::error::{err, err_str, parse_oid, thread_local};
use crate::identity::{Signature, signature_from_dto, to_signature};
use crate::object::kind_to_string;
use crate::repository::Repository;

#[napi]
pub struct Tag {
    repo: Arc<gix::ThreadSafeRepository>,
    id: gix::ObjectId,
}

#[napi]
impl Tag {
    #[napi(getter)]
    pub fn name(&self) -> Result<String> {
        let r = thread_local(&self.repo);
        let tag = r.find_tag(self.id).map_err(|e| err("find_tag", e))?;
        let tag = tag.decode().map_err(|e| err("decode", e))?;
        Ok(String::from_utf8_lossy(&tag.name).into_owned())
    }

    #[napi(getter)]
    pub fn message(&self) -> Result<String> {
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
    pub fn target_id(&self) -> Result<String> {
        let r = thread_local(&self.repo);
        let tag = r.find_tag(self.id).map_err(|e| err("find_tag", e))?;
        let tag = tag.decode().map_err(|e| err("decode", e))?;
        Ok(tag.target.to_string())
    }

    /// The kind of the object this tag points to (`commit`, `tree`, `blob`, or `tag`).
    #[napi(getter)]
    pub fn target_kind(&self) -> Result<String> {
        let r = thread_local(&self.repo);
        let tag = r.find_tag(self.id).map_err(|e| err("find_tag", e))?;
        let tag = tag.decode().map_err(|e| err("decode", e))?;
        Ok(kind_to_string(tag.target_kind))
    }

    /// The in-body cryptographic signature (armored, including its `-----BEGIN ... SIGNATURE-----`
    /// block) if the tag is signed, otherwise `null`. Presence only means the tag *carries* a
    /// signature; it does not prove the signature is valid (see [`Tag::verify`]).
    #[napi(getter)]
    pub fn signature(&self) -> Result<Option<String>> {
        let r = thread_local(&self.repo);
        let tag = r.find_tag(self.id).map_err(|e| err("find_tag", e))?;
        let tag = tag.decode().map_err(|e| err("decode", e))?;
        Ok(tag
            .signature
            .map(|s| String::from_utf8_lossy(&s).into_owned()))
    }

    /// The tagger identity, if this is an annotated tag.
    #[napi]
    pub fn tagger(&self) -> Result<Option<Signature>> {
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
    pub fn sign(&self) -> Result<Tag> {
        let r = thread_local(&self.repo);
        let options = crate::commit::signing_options(&r)?;
        let signed = r
            .find_tag(self.id)
            .map_err(|e| err("find_tag", e))?
            .decode()
            .map_err(|e| err("decode", e))?
            .sign(options)
            .map_err(|e| err("sign", e))?;
        let id = r
            .write_object(&signed)
            .map_err(|e| err("write_object", e))?;
        let signed_tag = r.find_tag(id).map_err(|e| err("find_tag", e))?;
        Ok(Tag {
            repo: Arc::clone(&self.repo),
            id: signed_tag.id,
        })
    }

    /// Verify this tag's signature.
    ///
    /// Returns `null` if the tag is not signed. Otherwise returns the verification outcome
    /// (`format`, `status`, `trustLevel`, `signer`, `key`, `fingerprint`, `output`, `valid`, ...).
    /// This shells out to the configured GPG/SSH/X.509 program, so it can be slow.
    #[napi]
    pub fn verify(&self) -> Result<Option<VerifyOutcome>> {
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
pub fn build_tag(repo: &Arc<gix::ThreadSafeRepository>, tag: gix::Tag) -> Result<Tag> {
    Ok(Tag {
        repo: Arc::clone(repo),
        id: tag.id,
    })
}

/// Input for writing an annotated tag object.
#[derive(Clone)]
#[napi(object)]
pub struct TagInput {
    pub name: String,
    pub target: String,
    pub target_kind: String,
    pub message: String,
    pub tagger: Option<Signature>,
    /// Optional pre-computed in-body signature (armored) to embed in the tag object.
    pub signature: Option<String>,
}

#[napi]
impl Repository {
    /// Find and decode a tag by id.
    #[napi]
    pub fn find_tag(&self, id: String) -> Result<Tag> {
        let r = thread_local(&self.inner);
        let tag = r
            .find_tag(parse_oid(&id)?)
            .map_err(|e| err("find_tag", e))?;
        build_tag(&self.inner, tag)
    }

    /// Write an annotated tag object directly to the object database.
    #[napi]
    pub fn write_tag(&self, input: TagInput) -> Result<String> {
        let r = thread_local(&self.inner);
        let target = parse_oid(&input.target)?;
        let target_kind = match input.target_kind.to_lowercase().as_str() {
            "commit" => gix::objs::Kind::Commit,
            "tree" => gix::objs::Kind::Tree,
            "blob" => gix::objs::Kind::Blob,
            "tag" => gix::objs::Kind::Tag,
            _ => {
                return Err(err_str(
                    "write_tag",
                    format!("invalid target_kind {}", input.target_kind),
                ));
            }
        };

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

        let oid = r.write_object(&tag).map_err(|e| err("write_tag", e))?;
        Ok(oid.to_string())
    }
}
