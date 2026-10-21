use napi::Result;
use napi_derive::napi;

use crate::error::err;
use crate::object::Signature;

/// Signature verification status and details.
#[derive(Clone, Debug)]
#[napi(object)]
pub struct SignatureVerificationOutcome {
    pub status: String,
    pub trust_level: Option<String>,
    pub is_valid: bool,
    pub message: Option<String>,
}

/// Options for `describe`.
#[derive(Clone, Debug, Default)]
#[napi(object)]
pub struct DescribeOptions {
    /// Select ref kind: "annotated-tags", "all-tags", or "all-refs".
    pub select: Option<String>,
    /// Traversal follows first-parent only.
    pub first_parent: Option<bool>,
    /// Maximum candidates to consider (default: 10).
    pub max_candidates: Option<u32>,
    /// Fallback to hex commit ID if no tag matches.
    pub id_as_fallback: Option<bool>,
}

/// Convert a JavaScript `Signature` DTO into `gix::actor::Signature`.
pub fn signature_to_gix(s: &Signature) -> gix::actor::Signature {
    gix::actor::Signature {
        name: s.name.clone().into(),
        email: s.email.clone().into(),
        time: gix::date::Time {
            seconds: s.time,
            offset: s.timezone_offset,
        },
    }
}

/// Convert `gix::actor::SignatureRef` into `Signature` DTO.
pub fn signature_from_gix_ref(s: gix::actor::SignatureRef<'_>) -> Signature {
    Signature {
        name: String::from_utf8_lossy(s.name).into_owned(),
        email: String::from_utf8_lossy(s.email).into_owned(),
        time: s.time().ok().map(|t| t.seconds).unwrap_or(0),
        timezone_offset: s.time().ok().map(|t| t.offset).unwrap_or(0),
    }
}

/// Verify signature on a gix::Commit.
pub fn verify_commit_signature(commit: &gix::Commit<'_>) -> Result<Option<SignatureVerificationOutcome>> {
    match commit.verify_signature() {
        Ok(Some(outcome)) => {
            let is_valid = outcome.is_valid();
            let status_str = format!("{:?}", outcome.status);
            let trust_str = format!("{:?}", outcome.trust_level);
            Ok(Some(SignatureVerificationOutcome {
                status: status_str,
                trust_level: Some(trust_str),
                is_valid,
                message: None,
            }))
        }
        Ok(None) => Ok(None),
        Err(e) => Err(err("verify_signature", e)),
    }
}

/// Describe a gix::Commit.
pub fn describe_commit(
    commit: &gix::Commit<'_>,
    options: Option<DescribeOptions>,
) -> Result<Option<String>> {
    let opts = options.unwrap_or_default();
    let mut platform = commit.describe();

    if let Some(ref select) = opts.select {
        match select.to_lowercase().as_str() {
            "all-tags" | "alltags" => {
                platform = platform.names(gix::commit::describe::SelectRef::AllTags);
            }
            "all-refs" | "allrefs" => {
                platform = platform.names(gix::commit::describe::SelectRef::AllRefs);
            }
            _ => {
                platform = platform.names(gix::commit::describe::SelectRef::AnnotatedTags);
            }
        }
    }

    if let Some(fp) = opts.first_parent {
        platform = platform.traverse_first_parent(fp);
    }

    if let Some(max) = opts.max_candidates {
        platform = platform.max_candidates(max as usize);
    }

    if let Some(fallback) = opts.id_as_fallback {
        platform = platform.id_as_fallback(fallback);
    }

    match platform.try_format() {
        Ok(Some(fmt)) => Ok(Some(fmt.to_string())),
        Ok(None) => Ok(None),
        Err(e) => Err(err("describe", e)),
    }
}
