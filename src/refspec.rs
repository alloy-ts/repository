use napi_derive::napi;

use crate::error::{err, err_str, parse_oid};

#[napi]
pub enum RefSpecOperation {
    /// The `src` side is local and the `dst` side is remote.
    Push,
    /// The `src` side is remote and the `dst` side is local.
    Fetch,
}

pub(crate) fn to_refspec_operation(op: RefSpecOperation) -> gix::refspec::parse::Operation {
    match op {
        RefSpecOperation::Push => gix::refspec::parse::Operation::Push,
        RefSpecOperation::Fetch => gix::refspec::parse::Operation::Fetch,
    }
}

fn bstr_to_string(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// The operation (fetch/push) of `spec` as the napi enum.
fn op_napi(spec: &gix::refspec::RefSpec) -> RefSpecOperation {
    match spec.to_ref().instruction() {
        gix::refspec::Instruction::Push(_) => RefSpecOperation::Push,
        gix::refspec::Instruction::Fetch(_) => RefSpecOperation::Fetch,
    }
}

/// A match input with `name` as its full reference name and a zero object id. Matching only
/// inspects the name, so the id is irrelevant.
fn match_item(name: &str) -> MatchItem {
    MatchItem {
        full_ref_name: name.to_string(),
        target: "0000000000000000000000000000000000000000".to_string(),
        object: None,
    }
}

/// The flattened instruction derived from a [`RefSpec`], making clear what to do with it.
#[napi(object)]
pub struct RefSpecInstruction {
    /// Which side of the spec is local: `"push"` or `"fetch"`.
    pub operation: String,
    /// The concrete instruction kind: `all_matching_branches`, `delete`, `matching`,
    /// `exclude`, `only`, or `and_update`.
    pub kind: String,
    /// The source expression, where applicable.
    pub src: Option<String>,
    /// The destination expression, where applicable.
    pub dst: Option<String>,
    /// Whether non-fast-forward updates of the destination are allowed.
    pub allow_non_fast_forward: bool,
}

fn to_instruction(i: gix::refspec::Instruction<'_>) -> RefSpecInstruction {
    use gix::refspec::Instruction as I;
    use gix::refspec::instruction::{Fetch, Push};
    match i {
        I::Push(Push::AllMatchingBranches {
            allow_non_fast_forward,
        }) => RefSpecInstruction {
            operation: "push".into(),
            kind: "all_matching_branches".into(),
            src: None,
            dst: None,
            allow_non_fast_forward,
        },
        I::Push(Push::Delete { ref_or_pattern }) => RefSpecInstruction {
            operation: "push".into(),
            kind: "delete".into(),
            src: None,
            dst: Some(bstr_to_string(ref_or_pattern)),
            allow_non_fast_forward: false,
        },
        I::Push(Push::Matching {
            src,
            dst,
            allow_non_fast_forward,
        }) => RefSpecInstruction {
            operation: "push".into(),
            kind: "matching".into(),
            src: Some(bstr_to_string(src)),
            dst: Some(bstr_to_string(dst)),
            allow_non_fast_forward,
        },
        I::Push(Push::Exclude { src }) | I::Fetch(Fetch::Exclude { src }) => RefSpecInstruction {
            operation: if matches!(i, I::Push(_)) {
                "push".into()
            } else {
                "fetch".into()
            },
            kind: "exclude".into(),
            src: Some(bstr_to_string(src)),
            dst: None,
            allow_non_fast_forward: false,
        },
        I::Fetch(Fetch::Only { src }) => RefSpecInstruction {
            operation: "fetch".into(),
            kind: "only".into(),
            src: Some(bstr_to_string(src)),
            dst: None,
            allow_non_fast_forward: false,
        },
        I::Fetch(Fetch::AndUpdate {
            src,
            dst,
            allow_non_fast_forward,
        }) => RefSpecInstruction {
            operation: "fetch".into(),
            kind: "and_update".into(),
            src: Some(bstr_to_string(src)),
            dst: Some(bstr_to_string(dst)),
            allow_non_fast_forward,
        },
    }
}

/// A parsed and validated git refspec, like `+refs/heads/*:refs/remotes/origin/*`.
///
/// Build one with [`RefSpec.parse()`] (static factory in JS), then inspect its sides,
/// derived instruction, and prefixes.
#[napi]
pub struct RefSpec {
    inner: gix::refspec::RefSpec,
}

#[napi]
impl RefSpec {
    /// Parse `spec` for use in `operation` and return it if it is valid.
    ///
    /// Patterns with more than one `*` or otherwise invalid ref names reject the parse.
    /// An empty fetch spec parses as a `HEAD`-only spec, matching Git.
    #[napi(factory)]
    pub fn parse(spec: String, operation: RefSpecOperation) -> napi::Result<RefSpec> {
        let parsed = gix::refspec::parse(
            gix::bstr::BStr::new(spec.as_bytes()),
            to_refspec_operation(operation),
        )
        .map_err(|e| err("refspec.parse", e))?;
        Ok(RefSpec {
            inner: parsed.to_owned(),
        })
    }

    /// Reproduce this refspec in parseable form.
    #[napi(getter)]
    pub fn spec(&self) -> String {
        bstr_to_string(self.inner.to_ref().to_bstring().as_slice())
    }

    /// Which side of the spec is local.
    #[napi(getter)]
    pub fn operation(&self) -> RefSpecOperation {
        match self.inner.to_ref().instruction() {
            gix::refspec::Instruction::Push(_) => RefSpecOperation::Push,
            gix::refspec::Instruction::Fetch(_) => RefSpecOperation::Fetch,
        }
    }

    /// How the spec was parsed: `"normal"`, `"force"` (starts with `+`), or `"negative"`
    /// (starts with `^`).
    #[napi(getter)]
    pub fn mode(&self) -> String {
        match self.inner.to_ref().instruction() {
            gix::refspec::Instruction::Fetch(gix::refspec::instruction::Fetch::Exclude {
                ..
            })
            | gix::refspec::Instruction::Push(gix::refspec::instruction::Push::Exclude {
                ..
            }) => "negative".into(),
            _ if self.inner.allow_non_fast_forward() => "force".into(),
            _ => "normal".into(),
        }
    }

    /// True if the spec starts with `+` and thus forces setting the reference.
    #[napi]
    pub fn allow_non_fast_forward(&self) -> bool {
        self.inner.allow_non_fast_forward()
    }

    /// The left-hand side of the spec, typically the source.
    ///
    /// It takes many different forms so don't rely on this being a ref name.
    /// It's not present in case of deletions.
    #[napi(getter)]
    pub fn source(&self) -> Option<String> {
        self.inner.to_ref().source().map(|b| bstr_to_string(b))
    }

    /// The right-hand side of the spec, typically the destination ref name or ref pattern.
    ///
    /// It's not present in case of source-only specs.
    #[napi(getter)]
    pub fn destination(&self) -> Option<String> {
        self.inner.to_ref().destination().map(|b| bstr_to_string(b))
    }

    /// The explicitly stored remote side, whose position depends on how the refspec was parsed.
    ///
    /// A one-sided push refspec has no explicit remote side and returns `null` here, even
    /// though its source is later also used as its destination.
    #[napi(getter)]
    pub fn remote(&self) -> Option<String> {
        self.inner.to_ref().remote().map(|b| bstr_to_string(b))
    }

    /// The explicitly stored local side, whose position depends on how the refspec was parsed.
    #[napi(getter)]
    pub fn local(&self) -> Option<String> {
        self.inner.to_ref().local().map(|b| bstr_to_string(b))
    }

    /// The prefix derived from the source side of this spec if this is a fetch spec, or the
    /// destination side if it is a push spec, if it is possible to do so without ambiguity.
    ///
    /// Exact refs starting with `refs/` are returned unchanged; ref patterns return the fixed
    /// portion before their single `*`. Negative refspecs and one-sided push refspecs
    /// return `null`.
    #[napi]
    pub fn prefix(&self) -> Option<String> {
        self.inner.to_ref().prefix().map(|b| bstr_to_string(b))
    }

    /// The remote-ref prefixes represented by this refspec, suitable for limiting the refs
    /// requested from a remote. Unlike [`prefix()`](Self::prefix), partial names without an
    /// unambiguous prefix are expanded to all of their Git-style ref-name candidates, e.g.
    /// `main` expands to `main`, `refs/main`, `refs/tags/main`, `refs/heads/main`,
    /// `refs/remotes/main`, and `refs/remotes/main/HEAD`.
    #[napi]
    pub fn expand_prefixes(&self) -> Vec<String> {
        let mut out = Vec::new();
        self.inner.to_ref().expand_prefixes(&mut out);
        out.iter().map(|b| bstr_to_string(b)).collect()
    }

    /// Transform the state of the refspec into an instruction making clear what to do with it.
    #[napi]
    pub fn instruction(&self) -> RefSpecInstruction {
        to_instruction(self.inner.to_ref().instruction())
    }

    /// Whether `name` is matched by the source (left-hand) side of this spec, like git2's
    /// `src_matches`.
    ///
    /// Note: a negative refspec never produces a mapping, so this returns `false` for names it
    /// excludes; its pattern is still visible via [`spec()`](Self::spec).
    #[napi]
    pub fn source_matches(&self, name: String) -> napi::Result<bool> {
        let group = MatchGroup::from_specs(vec![self], op_napi(&self.inner));
        Ok(!group.match_lhs(vec![match_item(&name)])?.is_empty())
    }

    /// Whether `name` is matched by the destination (right-hand) side of this spec, like git2's
    /// `dst_matches`.
    #[napi]
    pub fn destination_matches(&self, name: String) -> napi::Result<bool> {
        let group = MatchGroup::from_specs(vec![self], op_napi(&self.inner));
        Ok(!group.match_rhs(vec![match_item(&name)])?.is_empty())
    }

    /// Map `name` on the source side to its destination, like git2's `transform`. Returns `null`
    /// when `name` is not matched by this spec.
    #[napi]
    pub fn transform(&self, name: String) -> napi::Result<Option<String>> {
        let group = MatchGroup::from_specs(vec![self], op_napi(&self.inner));
        Ok(group
            .match_lhs(vec![match_item(&name)])?
            .into_iter()
            .next()
            .and_then(|m| m.rhs))
    }

    /// Reverse-map `name` on the destination side to its source, like git2's `rtransform`. Returns
    /// `null` when `name` is not matched by this spec.
    #[napi]
    pub fn rtransform(&self, name: String) -> napi::Result<Option<String>> {
        let group = MatchGroup::from_specs(vec![self], op_napi(&self.inner));
        Ok(group
            .match_rhs(vec![match_item(&name)])?
            .into_iter()
            .next()
            .map(|m| m.lhs))
    }
}

fn spec_operation(spec: &gix::refspec::RefSpec) -> gix::refspec::parse::Operation {
    match spec.to_ref().instruction() {
        gix::refspec::Instruction::Push(_) => gix::refspec::parse::Operation::Push,
        gix::refspec::Instruction::Fetch(_) => gix::refspec::parse::Operation::Fetch,
    }
}

/// An item to match against a [`MatchGroup`], input to the matching operations.
#[napi(object)]
pub struct MatchItem {
    /// The full name of the reference, like `refs/heads/main`.
    pub full_ref_name: String,
    /// The hex object id that `full_ref_name` points to, typically a commit but possibly
    /// a tag object (or anything else).
    pub target: String,
    /// The hex object id an annotated tag is pointing to, if `target` is an annotated tag.
    pub object: Option<String>,
}

/// A mapping from a remote ref to a local ref for fetches, or from a local ref to a
/// remote ref for pushes.
#[napi(object)]
pub struct RefSpecMapping {
    /// The index into the `items` list passed to the matching operation that matched,
    /// or `null` for object-id mappings emitted without matching.
    pub item_index: Option<u32>,
    /// The name of the remote side for fetches or the local one for pushes that matched.
    /// Either a full ref name or a hex object id.
    pub lhs: String,
    /// Whether `lhs` is an object id rather than a full ref name.
    pub lhs_is_object_id: bool,
    /// The local side for fetches or the remote side for pushes corresponding to `lhs`.
    pub rhs: Option<String>,
    /// The index of the matched refspec within the match group.
    pub spec_index: u32,
}

/// A fix applied while validating matched mappings.
#[napi(object)]
pub struct MatchFix {
    /// The destination ref name that was ignored because it was partial.
    pub name: String,
    /// The refspec that defined the removed mapping, in parseable form.
    pub spec: String,
}

/// The result of a validated match operation: fixed-up mappings and applied fixes.
/// Terminal issues (conflicting destinations) are surfaced as a thrown error whose
/// message lists them.
#[napi(object)]
pub struct ValidatedMatch {
    /// The mappings that remain after validation.
    pub mappings: Vec<RefSpecMapping>,
    /// Fixes applied during validation.
    pub fixes: Vec<MatchFix>,
}

fn parse_ids(items: &[MatchItem]) -> napi::Result<Vec<(gix::ObjectId, Option<gix::ObjectId>)>> {
    items
        .iter()
        .map(|i| {
            Ok((
                parse_oid(&i.target)?,
                i.object.as_deref().map(parse_oid).transpose()?,
            ))
        })
        .collect()
}

fn to_items<'a>(
    items: &'a [MatchItem],
    ids: &'a [(gix::ObjectId, Option<gix::ObjectId>)],
) -> Vec<gix::refspec::match_group::Item<'a>> {
    items
        .iter()
        .zip(ids.iter())
        .map(|(i, (target, object))| gix::refspec::match_group::Item {
            full_ref_name: gix::bstr::BStr::new(i.full_ref_name.as_bytes()),
            target,
            object: object.as_ref().map(|v| &**v),
        })
        .collect()
}

fn to_mapping(m: &gix::refspec::match_group::Mapping<'_, '_>) -> RefSpecMapping {
    use gix::refspec::match_group::SourceRef;
    let (lhs, lhs_is_object_id) = match &m.lhs {
        SourceRef::FullName(name) => (String::from_utf8_lossy(name).into_owned(), false),
        SourceRef::ObjectId(id) => (id.to_string(), true),
    };
    RefSpecMapping {
        item_index: m.item_index.map(|i| i as u32),
        lhs,
        lhs_is_object_id,
        rhs: m
            .rhs
            .as_ref()
            .map(|r| String::from_utf8_lossy(r).into_owned()),
        spec_index: m.spec_index as u32,
    }
}

/// Match a list of refspecs in order while handling negation, conflicts, and
/// one-to-many mappings.
#[napi]
pub struct MatchGroup {
    specs: Vec<gix::refspec::RefSpec>,
}

#[napi]
impl MatchGroup {
    /// Create a match group from already-parsed refspecs, keeping only those whose
    /// operation matches `operation` (fetch specs for fetch, push specs for push),
    /// like `from_fetch_specs` / `from_push_specs`.
    #[napi(factory)]
    pub fn from_specs(specs: Vec<&RefSpec>, operation: RefSpecOperation) -> MatchGroup {
        let op = to_refspec_operation(operation);
        MatchGroup {
            specs: specs
                .iter()
                .filter(|s| spec_operation(&s.inner) == op)
                .map(|s| s.inner.clone())
                .collect(),
        }
    }

    /// Parse each refspec in `specs` for use in `operation` and build a match group
    /// from the valid ones.
    #[napi(factory)]
    pub fn parse_specs(
        specs: Vec<String>,
        operation: RefSpecOperation,
    ) -> napi::Result<MatchGroup> {
        let op = to_refspec_operation(operation);
        let mut out = Vec::with_capacity(specs.len());
        for spec in &specs {
            out.push(
                gix::refspec::parse(gix::bstr::BStr::new(spec.as_bytes()), op)
                    .map_err(|e| err("refspec.parse", e))?
                    .to_owned(),
            );
        }
        Ok(MatchGroup { specs: out })
    }

    fn group(&self) -> gix::refspec::MatchGroup<'_> {
        gix::refspec::MatchGroup {
            specs: self.specs.iter().map(|s| s.to_ref()).collect(),
        }
    }

    /// Match all `items` against all fetch specs present in this group, returning
    /// deduplicated mappings from source to destination.
    ///
    /// `items` are expected to be references on the remote, which will be matched and
    /// mapped to obtain their local counterparts. Object IDs are emitted directly without
    /// being matched against `items`. Negative matches are not observable in the result
    /// but are used to remove mappings.
    #[napi]
    pub fn match_lhs(&self, items: Vec<MatchItem>) -> napi::Result<Vec<RefSpecMapping>> {
        let ids = parse_ids(&items)?;
        let gix_items = to_items(&items, &ids);
        Ok(self
            .group()
            .match_lhs(gix_items.iter().copied())
            .mappings
            .iter()
            .map(to_mapping)
            .collect())
    }

    /// Match all `items` against the right-hand side of all fetch specs present in this
    /// group, returning deduplicated mappings from destination to source.
    ///
    /// `items` are expected to be tracking references in the local clone, which will be
    /// matched and reverse-mapped to obtain their remote counterparts. Negative fetch
    /// refspecs exclude mappings by their source side.
    #[napi]
    pub fn match_rhs(&self, items: Vec<MatchItem>) -> napi::Result<Vec<RefSpecMapping>> {
        let ids = parse_ids(&items)?;
        let gix_items = to_items(&items, &ids);
        Ok(self
            .group()
            .match_rhs(gix_items.iter().copied())
            .mappings
            .iter()
            .map(to_mapping)
            .collect())
    }

    /// Like [`match_lhs`](Self::match_lhs), but validates the mappings: conflicting
    /// destinations are returned as `issues`, and mappings with partial destinations
    /// are removed and reported in `fixes`.
    #[napi(ts_return_type = "ValidatedMatch")]
    pub fn match_lhs_validated(&self, items: Vec<MatchItem>) -> napi::Result<ValidatedMatch> {
        use gix::refspec::match_group::validate::Fix;
        let ids = parse_ids(&items)?;
        let gix_items = to_items(&items, &ids);
        let (outcome, fixes) = self
            .group()
            .match_lhs(gix_items.iter().copied())
            .validated()
            .map_err(|e| err_str("match_lhs_validated", e))?;
        Ok(ValidatedMatch {
            mappings: outcome.mappings.iter().map(to_mapping).collect(),
            fixes: fixes
                .into_iter()
                .map(|f| match f {
                    Fix::MappingWithPartialDestinationRemoved { name, spec } => MatchFix {
                        name: bstr_to_string(&name),
                        spec: bstr_to_string(spec.to_ref().to_bstring().as_slice()),
                    },
                })
                .collect(),
        })
    }
}
