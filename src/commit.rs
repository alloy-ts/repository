#![allow(clippy::empty_docs)]

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

use napi::Result;
use napi_derive::napi;

use gix::bstr::ByteSlice;
use gix::config::tree::{Gpg, User, gpg};
use gix::objs::signature::sign::Options as SignOptions;
use gix::objs::signature::sign::is_literal_ssh_key;
use gix::objs::signature::verify::{Outcome, TrustLevel};
use gix::objs::signature::{self, Format};

use crate::error::{err, parse_oid, thread_local};
use crate::object::{Signature, to_signature};
use crate::repository::{Repository, signature_from_dto};

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// A git commit exposed to TypeScript.
#[napi]
pub struct Commit {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) id: gix::ObjectId,
}

#[napi]
impl Commit {
    /// The object id of this commit.
    #[napi(getter)]
    pub fn id(&self) -> String {
        self.id.to_string()
    }

    /// Full commit message.
    #[napi(getter)]
    pub fn message(&self) -> Result<String> {
        let r = thread_local(&self.repo);
        let commit = r.find_commit(self.id).map_err(|e| err("find_commit", e))?;
        let commit = commit.decode().map_err(|e| err("decode", e))?;
        Ok(lossy(commit.message))
    }

    /// The root tree id of the commit.
    #[napi(getter)]
    pub fn tree_id(&self) -> Result<String> {
        let r = thread_local(&self.repo);
        let commit = r.find_commit(self.id).map_err(|e| err("find_commit", e))?;
        let commit = commit.decode().map_err(|e| err("decode", e))?;
        Ok(commit.tree.to_string())
    }

    /// Parent commit ids.
    #[napi(getter)]
    pub fn parent_ids(&self) -> Result<Vec<String>> {
        let r = thread_local(&self.repo);
        let commit = r.find_commit(self.id).map_err(|e| err("find_commit", e))?;
        let commit = commit.decode().map_err(|e| err("decode", e))?;
        Ok(commit.parents().map(|p| p.to_string()).collect())
    }

    /// The author identity of this commit.
    #[napi]
    pub fn author(&self) -> Result<Signature> {
        let r = thread_local(&self.repo);
        let commit = r.find_commit(self.id).map_err(|e| err("find_commit", e))?;
        let commit = commit.decode().map_err(|e| err("decode", e))?;
        let sig = commit.author().map_err(|e| err("author", e))?;
        let time = sig.time().map_err(|e| err("author.time", e))?;
        Ok(to_signature(sig.name, sig.email, time.seconds, time.offset))
    }

    /// The committer identity of this commit.
    #[napi]
    pub fn committer(&self) -> Result<Signature> {
        let r = thread_local(&self.repo);
        let commit = r.find_commit(self.id).map_err(|e| err("find_commit", e))?;
        let commit = commit.decode().map_err(|e| err("decode", e))?;
        let sig = commit.committer().map_err(|e| err("committer", e))?;
        let time = sig.time().map_err(|e| err("committer.time", e))?;
        Ok(to_signature(sig.name, sig.email, time.seconds, time.offset))
    }

    /// The raw cryptographic signature (the `gpgsig`/`gpgsig-sha256` header) if the commit is
    /// signed, otherwise `null`. Presence only means the commit *carries* a signature; it does not
    /// prove the signature is valid (see [`Commit::verify`]).
    #[napi(getter)]
    pub fn signature(&self) -> Result<Option<String>> {
        let r = thread_local(&self.repo);
        let commit = r.find_commit(self.id).map_err(|e| err("find_commit", e))?;
        let sig = commit.signature().map_err(|e| err("signature", e))?;
        Ok(sig.map(|(s, _)| lossy(&*s)))
    }

    /// Verify this commit's signature.
    ///
    /// Returns `null` if the commit is not signed. Otherwise returns the verification outcome
    /// (`status`, `trustLevel`, `signer`). This shells out to the configured GPG/SSH/X.509 program,
    /// so it can be slow.
    #[napi]
    pub fn verify(&self) -> Result<Option<VerifyOutcome>> {
        let r = thread_local(&self.repo);
        let commit = r.find_commit(self.id).map_err(|e| err("find_commit", e))?;
        let Some((signature, signed_data)) = commit.signature().map_err(|e| err("signature", e))?
        else {
            return Ok(None);
        };

        let config = r.config_snapshot();
        let minimum_trust = config
            .string(Gpg::MIN_TRUST_LEVEL)
            .map(|v| Gpg::MIN_TRUST_LEVEL.try_into_trust_level(v))
            .transpose()
            .map_err(|e| err("gpg.minTrustLevel", e))?
            .unwrap_or(TrustLevel::Undefined);

        let format = Format::from_signature(&*signature)
            .ok_or_else(|| err("verify", "unsupported or unrecognized signature format"))?;

        let program = signature_program(&config, format)?;

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
                        err(
                            "verify",
                            "gpg.ssh.allowedSignersFile must be configured for SSH signature verification",
                        )
                    })?;
                let revocation_file = config
                    .trusted_path(gpg::Ssh::REVOCATION_FILE)
                    .map_err(|e| err("gpg.ssh.revocationFile", e))?
                    .filter(|p| p.exists());
                signature::verify::Options::Ssh {
                    program,
                    program_arguments: Vec::new(),
                    environment: Vec::new(),
                    allowed_signers: resolve_relative_to_repository(&r, allowed_signers),
                    revocation_file: revocation_file.map(|p| resolve_relative_to_repository(&r, p)),
                    verify_time: commit.time().map_err(|e| err("commit.time", e))?,
                    minimum_trust,
                }
            }
        };

        let outcome = signed_data
            .verify(&*signature, options)
            .map_err(|e| err("verify", e))?;
        Ok(Some(VerifyOutcome::from(outcome)))
    }

    /// Sign this commit, producing a *new* commit object that carries the signature, and return it.
    ///
    /// The signing key and program are taken from the repository configuration
    /// (`user.signingKey`, `gpg.format`, `gpg.<format>.program`, etc.), mirroring `git commit -S`.
    #[napi]
    pub fn sign(&self) -> Result<Commit> {
        let r = thread_local(&self.repo);
        let commit = r.find_commit(self.id).map_err(|e| err("find_commit", e))?;
        let options = signing_options(&r)?;
        let signed = commit
            .decode()
            .map_err(|e| err("decode", e))?
            .sign(options)
            .map_err(|e| err("sign", e))?;
        let id = r
            .write_object(&signed)
            .map_err(|e| err("write_object", e))?;
        let signed_commit = r.find_commit(id).map_err(|e| err("find_commit", e))?;
        Ok(Commit {
            repo: Arc::clone(&self.repo),
            id: signed_commit.id,
        })
    }
}

/// The result of verifying a commit signature.
#[napi(object)]
pub struct VerifyOutcome {
    /// `Good`, `Bad`, or `Unknown`.
    pub status: String,
    /// `Undefined`, `Never`, `Marginal`, `Fully`, etc.
    pub trust_level: String,
    /// The signer identity, if known.
    pub signer: Option<String>,
}

impl From<Outcome> for VerifyOutcome {
    fn from(o: Outcome) -> Self {
        VerifyOutcome {
            status: format!("{:?}", o.status),
            trust_level: format!("{:?}", o.trust_level),
            signer: o.signer.as_ref().map(|s| lossy(s.as_slice())),
        }
    }
}

/// Construct a [`Commit`] wrapper from a gix commit (used by `Repository` / `GitObject`).
pub fn build_commit(repo: &Arc<gix::ThreadSafeRepository>, commit: gix::Commit) -> Result<Commit> {
    Ok(Commit {
        repo: Arc::clone(repo),
        id: commit.id,
    })
}

/// Resolve the signing/verification program for the given signature format, falling back to the
/// format's configured default (`gpg` / `gpgsm` / `ssh-keygen`) when no explicit program is set.
fn signature_program(config: &gix::config::Snapshot, format: Format) -> Result<OsString> {
    let (configured, default) = match format {
        Format::OpenPgp => (config.string(gpg::OpenPgp::PROGRAM), "gpg"),
        Format::X509 => (config.string(gpg::X509::PROGRAM), "gpgsm"),
        Format::Ssh => (config.string(gpg::Ssh::PROGRAM), "ssh-keygen"),
    };
    match configured {
        Some(program) => Ok(OsString::from(lossy(program.as_slice()))),
        None => Ok(OsString::from(default)),
    }
}

/// Compute the [`SignOptions`] for signing, resolving the signing key and program from the
/// repository configuration — equivalent to `git commit -S`.
fn signing_options(repo: &gix::Repository) -> Result<SignOptions> {
    let config = repo.config_snapshot();
    let value = config.string(Gpg::FORMAT);
    let format = Gpg::FORMAT
        .try_into_signature_format(value.as_ref().map(|v| v.as_bstr()))
        .map_err(|e| err("gpg.format", e))?;
    let program = signature_program(&config, format)?;

    let signing_key = match config.string(User::SIGNING_KEY) {
        Some(key) if !key.is_empty() => gix::path::from_bstring(key)
            .map(|p| p.into_os_string())
            .map_err(|e| err("user.signingKey", e))?,
        _ if format == Format::Ssh => default_ssh_key(&config)?.ok_or_else(|| {
            err(
                "user.signingKey",
                "user.signingKey or gpg.ssh.defaultKeyCommand must provide an SSH signing key",
            )
        })?,
        _ => {
            let committer = repo
                .committer()
                .ok_or_else(|| {
                    err(
                        "committer",
                        "Committer identity is not configured and user.signingKey is unset",
                    )
                })?
                .map_err(|e| err("committer", e))?;
            let mut identity = committer.name.to_vec();
            identity.extend_from_slice(b" <");
            identity.extend_from_slice(committer.email);
            identity.extend_from_slice(b">");
            gix::path::from_bstring(identity)
                .map(|p| p.into_os_string())
                .map_err(|e| err("committer", e))?
        }
    };

    Ok(SignOptions {
        format,
        program,
        program_arguments: Vec::new(),
        signing_key,
        environment: Vec::new(),
    })
}

/// Resolve `gpg.ssh.defaultKeyCommand` to an SSH signing key when `user.signingKey` is unset.
fn default_ssh_key(config: &gix::config::Snapshot) -> Result<Option<OsString>> {
    let Some(program) = config.string(gpg::Ssh::DEFAULT_KEY_COMMAND) else {
        return Ok(None);
    };
    let program = lossy(program.as_slice());
    let output = std::process::Command::new(&program)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| err("gpg.ssh.defaultKeyCommand", e))?;
    if !output.status.success() {
        return Err(err(
            "gpg.ssh.defaultKeyCommand",
            "command exited with failure",
        ));
    }
    let key = output
        .stdout
        .as_bstr()
        .lines()
        .next()
        .unwrap_or_default()
        .trim();
    if is_literal_ssh_key(key).is_none() {
        return Err(err(
            "gpg.ssh.defaultKeyCommand",
            "returned an invalid (non-literal) SSH key",
        ));
    }
    Ok(Some(OsString::from(lossy(key.as_ref()))))
}

/// If `path` is relative, resolve it against the repository's working tree (or git dir).
fn resolve_relative_to_repository(repo: &gix::Repository, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        repo.workdir().unwrap_or_else(|| repo.git_dir()).join(path)
    }
}

/// Input for writing a loose commit object.
#[derive(Clone)]
#[napi(object)]
pub struct CommitInput {
    pub tree: String,
    pub parents: Vec<String>,
    pub message: String,
    pub author: Option<Signature>,
    pub committer: Option<Signature>,
}

/// Information about the commit-graph, if present.
#[napi(object)]
pub struct CommitGraphInfo {
    /// Number of commits recorded in the commit-graph.
    pub commit_count: f64,
}

#[napi]
impl Repository {
    /// Find and decode a commit by id.
    #[napi]
    pub fn find_commit(&self, id: String) -> Result<Commit> {
        let r = thread_local(&self.inner);
        let commit = r
            .find_commit(parse_oid(&id)?)
            .map_err(|e| err("find_commit", e))?;
        build_commit(&self.inner, commit)
    }

    /// Write a loose commit object directly to the object database without moving references.
    #[napi]
    pub fn write_commit(&self, input: CommitInput) -> Result<String> {
        let r = thread_local(&self.inner);
        let tree = parse_oid(&input.tree)?;
        let parents = input
            .parents
            .iter()
            .map(|p| parse_oid(p))
            .collect::<Result<Vec<_>>>()?;

        let default_author = r.author().and_then(|res| res.ok());
        let default_committer = r.committer().and_then(|res| res.ok());

        let author = match input.author {
            Some(s) => signature_from_dto(&s)?,
            None => default_author
                .map(|s| s.into())
                .ok_or_else(|| err("author", "no author configured"))?,
        };
        let committer = match input.committer {
            Some(s) => signature_from_dto(&s)?,
            None => default_committer
                .map(|s| s.into())
                .ok_or_else(|| err("committer", "no committer configured"))?,
        };

        let commit = gix::objs::Commit {
            tree,
            parents: parents.into(),
            author,
            committer,
            encoding: None,
            message: input.message.into(),
            extra_headers: Vec::new(),
        };

        let oid = r
            .write_object(&commit)
            .map_err(|e| err("write_commit", e))?;
        Ok(oid.to_string())
    }

    /// Create a commit on `reference` and return its id.
    #[napi]
    pub fn commit(
        &self,
        reference: String,
        message: String,
        tree: String,
        parents: Vec<String>,
    ) -> Result<String> {
        let r = thread_local(&self.inner);
        let parent_ids: Vec<gix::ObjectId> = parents
            .iter()
            .map(|p| parse_oid(p))
            .collect::<Result<Vec<_>>>()?;
        let id = r
            .commit(reference, message.as_str(), parse_oid(&tree)?, parent_ids)
            .map_err(|e| err("commit", e))?;
        Ok(id.to_string())
    }

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
}
