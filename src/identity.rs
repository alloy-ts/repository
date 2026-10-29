use napi_derive::napi;

use crate::error::{err, thread_local};
use crate::object::lossy;
use crate::repository::Repository;

/// A person with name and email (no timestamp), mirroring `gix_actor::Identity`.
#[derive(Clone, Default)]
#[napi(object)]
pub struct Identity {
    /// The actor's name.
    pub name: String,
    /// The actor's email.
    pub email: String,
}

/// A mutable signature that is created by an actor at a certain time,
/// mirroring `gix_actor::Signature`. Note that this is not a cryptographical signature.
#[derive(Clone)]
#[napi(object)]
pub struct Signature {
    pub name: String,
    pub email: String,
    pub time: i64,
    pub timezone_offset: i32,
}

/// Build a NAPI [`Signature`] from raw actor parts.
pub(crate) fn to_signature(name: &[u8], email: &[u8], seconds: i64, offset: i32) -> Signature {
    Signature {
        name: lossy(name),
        email: lossy(email),
        time: seconds,
        timezone_offset: offset,
    }
}

pub(crate) fn identity_from_actor(identity: gix::actor::IdentityRef<'_>) -> Identity {
    let identity = identity.trim();
    Identity {
        name: lossy(identity.name),
        email: lossy(identity.email),
    }
}

pub(crate) fn signature_from_actor(s: &gix::actor::SignatureRef<'_>) -> Signature {
    let time = s.time().ok();
    to_signature(
        s.name,
        s.email,
        time.as_ref().map(|t| t.seconds).unwrap_or_default(),
        time.as_ref().map(|t| t.offset).unwrap_or_default(),
    )
}

pub(crate) fn signature_from_dto(s: &Signature) -> napi::Result<gix::actor::Signature> {
    Ok(gix::actor::Signature {
        name: s.name.clone().into(),
        email: s.email.clone().into(),
        time: gix::date::Time {
            seconds: s.time,
            offset: s.timezone_offset,
        },
    })
}

/// The actor's name and email without the timestamp, mirroring
/// `From<gix_actor::Signature> for gix_actor::Identity`.
#[napi]
pub fn signature_to_identity(sig: Signature) -> Identity {
    Identity {
        name: sig.name,
        email: sig.email,
    }
}

/// Serialise a signature to the git actor format `Name <email> <seconds> <offset>`.
#[napi]
pub fn signature_to_raw(sig: Signature) -> napi::Result<String> {
    let s = signature_from_dto(&sig)?;
    let mut out = Vec::new();
    s.write_to(&mut out)
        .map_err(|e| err("signature.write_to", e))?;
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// Serialise an identity to the git identity format `Name <email>`
/// (a signature without timestamp).
#[napi]
pub fn identity_to_raw(identity: Identity) -> napi::Result<String> {
    let identity = gix::actor::Identity {
        name: identity.name.into(),
        email: identity.email.into(),
    };
    let mut out = Vec::new();
    identity
        .write_to(&mut out)
        .map_err(|e| err("identity.write_to", e))?;
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// Parse an identity from `data` (typically `Name <name@example.com> 1700000000 +0000`),
/// trimming surrounding whitespace. Returns `null` when nothing could be parsed.
#[napi]
pub fn parse_identity(data: String) -> Option<Identity> {
    gix::actor::IdentityRef::from_bytes(data.as_bytes())
        .ok()
        .map(identity_from_actor)
}

/// Parse a signature from `data` (typically `Name <name@example.com> 1700000000 +0000`),
/// trimming surrounding whitespace and decoding the timestamp. Returns `null` when
/// nothing could be parsed or the timestamp is malformed.
#[napi]
pub fn parse_signature(data: String) -> Option<Signature> {
    let sig = gix::actor::SignatureRef::from_bytes(data.as_bytes()).ok()?;
    Some(signature_from_actor(&sig.trim()))
}

#[napi]
impl Repository {
    /// Retrieve the default author signature configured for this repository.
    #[napi]
    pub fn default_author(&self) -> napi::Result<Option<Signature>> {
        let r = thread_local(&self.inner);
        Ok(r.author()
            .and_then(|res| res.ok())
            .map(|s| signature_from_actor(&s)))
    }

    /// Retrieve the default committer signature configured for this repository.
    #[napi]
    pub fn default_committer(&self) -> napi::Result<Option<Signature>> {
        let r = thread_local(&self.inner);
        Ok(r.committer()
            .and_then(|res| res.ok())
            .map(|s| signature_from_actor(&s)))
    }

    /// Return the configured committer, or install `name`/`email` as an in-memory
    /// fallback if none is configured, and return it.
    ///
    /// The fallback lives only on the underlying handle for the duration of this call;
    /// it is not persisted to configuration.
    #[napi]
    pub fn committer_or_set_fallback(
        &self,
        name: String,
        email: String,
    ) -> napi::Result<Signature> {
        let mut r = thread_local(&self.inner);
        let s = r
            .committer_or_set_fallback(name.as_bytes(), email.as_bytes())
            .map_err(|e| err("committer_or_set_fallback", e))?;
        Ok(signature_from_actor(&s))
    }

    /// Like `committerOrSetFallback`, but installs a generic fallback identity when
    /// nothing is configured.
    #[napi]
    pub fn committer_or_set_generic_fallback(&self) -> napi::Result<Signature> {
        let mut r = thread_local(&self.inner);
        let s = r
            .committer_or_set_generic_fallback()
            .map_err(|e| err("committer_or_set_generic_fallback", e))?;
        Ok(signature_from_actor(&s))
    }

    /// Return the configured author, or `name`/`email` as a fallback when no author identity is
    /// configured.
    ///
    /// Note: unlike the `committer` counterpart, this does **not** install the fallback into the
    /// repository configuration — gix 0.89 only supports installing committer fallbacks, so the
    /// synthesized identity is returned directly.
    #[napi]
    pub fn author_or_set_fallback(&self, name: String, email: String) -> napi::Result<Signature> {
        let r = thread_local(&self.inner);
        match r.author().and_then(|res| res.ok()) {
            Some(s) => Ok(signature_from_actor(&s)),
            None => Ok(to_signature(name.as_bytes(), email.as_bytes(), 0, 0)),
        }
    }

    /// Like `authorOrSetFallback`, but uses a generic fallback identity when nothing is configured.
    #[napi]
    pub fn author_or_set_generic_fallback(&self) -> napi::Result<Signature> {
        let r = thread_local(&self.inner);
        match r.author().and_then(|res| res.ok()) {
            Some(s) => Ok(signature_from_actor(&s)),
            None => Ok(to_signature(
                b"no name configured",
                b"noEmailAvailable@example.com",
                0,
                0,
            )),
        }
    }
}
