use std::sync::Arc;

use chrono::{FixedOffset, TimeZone, Utc};

use gix::objs::Write;
use napi::bindgen_prelude::{Buffer, Either};
use napi_derive::napi;

use crate::blob::{Blob, build_blob};
use crate::commit::Commit;
use crate::entry::Entry;
use crate::error::{err, err_str, parse_oid, thread_local};
use crate::replace::{ReplaceOptions, ReplaceResult};
use crate::repository::Repository;
use crate::tag::Tag;
use crate::tree::Tree;

/// The kind of a git object, mirroring libgit2's `ObjectType`.
///
/// The boundary values are the capitalised names — `"Commit"`, `"Tree"`, `"Blob"`, `"Tag"`,
/// and `"Any"`. The `Any` variant (only meaningful for the peel methods) follows tag chains
/// to the first object that is *not* a tag, matching git's `<rev>^{}`.
#[napi(string_enum)]
pub enum ObjectType {
    Commit,
    Tree,
    Blob,
    Tag,
    Any,
}

/// Map an [`ObjectType`] to a concrete gix object kind. `Any` is rejected here because it has
/// no concrete kind — it is handled separately by the peel methods.
fn to_object_kind(t: ObjectType) -> napi::Result<gix::object::Kind> {
    Ok(match t {
        ObjectType::Commit => gix::object::Kind::Commit,
        ObjectType::Tree => gix::object::Kind::Tree,
        ObjectType::Blob => gix::object::Kind::Blob,
        ObjectType::Tag => gix::object::Kind::Tag,
        ObjectType::Any => {
            return Err(err_str(
                "object_kind",
                "\"Any\" is only valid for the peel methods, not for a concrete object kind",
            ));
        }
    })
}

pub(crate) fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Decode object bytes into a `String` for a few common encodings. UTF-8 is the
/// default and also the fallback for unknown encodings (non-text bytes become the
/// Unicode replacement character).
pub(crate) fn decode_text(data: &[u8], encoding: Option<&str>) -> String {
    match encoding.unwrap_or("utf8") {
        "utf16le" | "utf-16le" => {
            let mut s = String::new();
            for chunk in data.chunks_exact(2) {
                let code = u16::from_le_bytes([chunk[0], chunk[1]]);
                s.push(char::from_u32(code as u32).unwrap_or('\u{FFFD}'));
            }
            s
        }
        "latin1" | "ascii" | "binary" | "iso-8859-1" => data.iter().map(|&b| b as char).collect(),
        _ => String::from_utf8_lossy(data).into_owned(),
    }
}

/// Format a git timestamp (unix `seconds` in UTC plus `offset` seconds) as an RFC 3339 / ISO 8601
/// string, e.g. `2024-01-02T03:04:05+00:00`. Returns an empty string for out-of-range instants.
pub(crate) fn rfc3339(seconds: i64, offset: i32) -> String {
    match Utc.timestamp_opt(seconds, 0).single() {
        Some(dt) => dt
            .with_timezone(
                &FixedOffset::east_opt(offset).unwrap_or_else(|| FixedOffset::east_opt(0).unwrap()),
            )
            .to_rfc3339(),
        None => String::new(),
    }
}

/// Abbreviated object id: the shortest prefix that is unambiguous within `r`'s object store, never
/// shorter than 7 characters (git's default `core.abbrev`). Falls back to the full id if a unique
/// prefix cannot be found. Mirrors `git rev-parse --short`.
pub(crate) fn short_sha(r: &gix::Repository, id: &gix::ObjectId) -> String {
    let hex = id.to_string();
    let full_len = hex.len();
    let min = 7;
    if full_len <= min {
        return hex;
    }
    for n in min..full_len {
        let prefix_hex = &hex[..n];
        let prefix = match gix::hash::Prefix::from_hex(prefix_hex) {
            Ok(p) => p,
            Err(_) => break,
        };
        if r.objects.lookup_prefix(prefix, None).is_ok() {
            return prefix_hex.to_string();
        }
    }
    hex
}

/// The body of a commit message: everything after the first blank line, with leading and trailing
/// whitespace trimmed. Tolerates CRLF line endings. Returns `None` when there is no body.
pub(crate) fn message_body(message: &[u8]) -> Option<String> {
    let mut offset = 0;
    while let Some(pos) = message[offset..].iter().position(|&c| c == b'\n') {
        let line_end = offset + pos + 1;
        let mut rest = line_end;
        if message.get(rest) == Some(&b'\r') {
            rest += 1;
        }
        if message.get(rest) == Some(&b'\n') {
            let body = message.get(rest + 1..).unwrap_or_default();
            let start = body
                .iter()
                .position(|&c| !c.is_ascii_whitespace())
                .unwrap_or(0);
            let end = body
                .iter()
                .rposition(|&c| !c.is_ascii_whitespace())
                .map(|i| i + 1)
                .unwrap_or(0);
            let body = &body[start..end];
            return if body.is_empty() {
                None
            } else {
                Some(lossy(body))
            };
        }
        offset = line_end;
    }
    None
}

/// A kinded wrapper around a git object, holding its decoded content plus a handle
/// to the repository it came from. The content-bearing `inner` field means this is
/// the "real" object (like `gix::objs::Object`), so it can be constructed from any
/// of the object types via the `fromBlob` / `fromCommit` / `fromTree` / `fromTag` /
/// `fromEntry` static constructors.
#[napi]
pub struct Object {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) id: gix::ObjectId,
    inner: gix::objs::Object,
}

/// Decode a found `gix::Object` into its owned, content-bearing form.
fn decode_object(
    obj: gix::Object<'_>,
    hash_kind: gix::hash::Kind,
) -> napi::Result<gix::objs::Object> {
    let detached = obj.detach();
    let obj_ref = gix::objs::ObjectRef::from_bytes(&detached.data, detached.kind, hash_kind)
        .map_err(|e| err("decode", e))?;
    obj_ref.to_owned().map_err(|e| err("decode", e))
}

/// Construct an [`Object`] by reading and decoding `id` from `repo`.
pub(crate) fn build_object(
    repo: &Arc<gix::ThreadSafeRepository>,
    id: gix::ObjectId,
) -> napi::Result<Object> {
    let r = thread_local(repo);
    let obj = r.find_object(id).map_err(|e| err("find_object", e))?;
    let inner = decode_object(obj, r.object_hash()).map_err(|e| err("decode", e))?;
    Ok(Object {
        repo: Arc::clone(repo),
        id,
        inner,
    })
}

#[napi]
impl Object {
    /// The object id of this object.
    #[napi(getter)]
    pub fn id(&self) -> String {
        self.id.to_string()
    }

    /// The object kind (`"blob"`, `"tree"`, `"commit"`, or `"tag"`).
    #[napi(getter)]
    pub fn kind(&self) -> String {
        match &self.inner {
            gix::objs::Object::Tree(_) => "tree",
            gix::objs::Object::Blob(_) => "blob",
            gix::objs::Object::Commit(_) => "commit",
            gix::objs::Object::Tag(_) => "tag",
        }
        .to_string()
    }

    /// The size of the object's encoded data in bytes, read from its header
    /// (cheap — no full decode). Mirrors `git cat-file -s`.
    #[napi(getter)]
    pub fn size(&self) -> napi::Result<f64> {
        Ok(self.find_header()?.size)
    }

    /// The object's kind as the [`ObjectType`] enum (for TS exhaustiveness),
    /// alongside the string form exposed by [`kind`](Self::kind).
    #[napi(getter)]
    pub fn object_type(&self) -> ObjectType {
        match &self.inner {
            gix::objs::Object::Tree(_) => ObjectType::Tree,
            gix::objs::Object::Blob(_) => ObjectType::Blob,
            gix::objs::Object::Commit(_) => ObjectType::Commit,
            gix::objs::Object::Tag(_) => ObjectType::Tag,
        }
    }

    /// The raw, decompressed object body (without the loose-object header).
    #[napi]
    pub fn data(&self) -> napi::Result<Buffer> {
        let r = thread_local(&self.repo);
        let id = crate::replace::resolve_replace_head(&self.repo, &self.id).unwrap_or(self.id);
        let obj = r
            .find_object(id)
            .map_err(|e| err("find_object", e))?
            .detach();
        Ok(Buffer::from(obj.data))
    }

    /// The object's kind and raw, decompressed body, as [`ObjectData`]. This is the
    /// instance-level mirror of [`Repository::read_object`].
    #[napi]
    pub fn content(&self) -> napi::Result<ObjectData> {
        Ok(ObjectData {
            kind: self.kind(),
            data: self.data()?,
        })
    }

    /// A text view of [`data`](Self::data). Decodes as UTF-8 by default; pass
    /// `"utf16le"` or `"latin1"`/`"ascii"` for other encodings.
    #[napi]
    pub fn text(&self, encoding: Option<String>) -> napi::Result<String> {
        let data = self.data()?;
        Ok(decode_text(&data, encoding.as_deref()))
    }

    /// Read the object's header (kind + size) without decoding its full content.
    #[napi]
    pub fn find_header(&self) -> napi::Result<ObjectHeader> {
        let r = thread_local(&self.repo);
        let header = r.find_header(self.id).map_err(|e| err("find_header", e))?;
        Ok(ObjectHeader::from(header))
    }

    /// Like [`find_header`](Self::find_header) but returns `null` if the object is missing.
    #[napi]
    pub fn try_find_header(&self) -> napi::Result<Option<ObjectHeader>> {
        let r = thread_local(&self.repo);
        match r.find_header(self.id) {
            Ok(header) => Ok(Some(ObjectHeader::from(header))),
            Err(_) => Ok(None),
        }
    }

    /// Return this object as a [`Commit`], or `null` if it is not a commit.
    #[napi]
    pub fn as_commit(&self) -> napi::Result<Option<Commit>> {
        Ok(match &self.inner {
            gix::objs::Object::Commit(_) => Some(Commit {
                repo: Arc::clone(&self.repo),
                id: self.id,
            }),
            _ => None,
        })
    }

    /// Return this object as a [`Tree`], or `null` if it is not a tree.
    #[napi]
    pub fn as_tree(&self) -> napi::Result<Option<Tree>> {
        Ok(match &self.inner {
            gix::objs::Object::Tree(_) => Some(Tree {
                repo: Arc::clone(&self.repo),
                id: self.id,
            }),
            _ => None,
        })
    }

    /// Return this object as a [`Blob`], or `null` if it is not a blob.
    #[napi]
    pub fn as_blob(&self) -> napi::Result<Option<Blob>> {
        Ok(match &self.inner {
            gix::objs::Object::Blob(_) => Some(Blob {
                repo: Arc::clone(&self.repo),
                id: self.id,
            }),
            _ => None,
        })
    }

    /// Return this object as a [`Tag`], or `null` if it is not a tag.
    #[napi]
    pub fn as_tag(&self) -> napi::Result<Option<Tag>> {
        Ok(match &self.inner {
            gix::objs::Object::Tag(_) => Some(Tag {
                repo: Arc::clone(&self.repo),
                id: self.id,
            }),
            _ => None,
        })
    }

    /// Follow tag/commit objects until a commit is reached, returning it.
    #[napi]
    pub fn peel_to_commit(&self) -> napi::Result<Commit> {
        let r = thread_local(&self.repo);
        let obj = r.find_object(self.id).map_err(|e| err("find_object", e))?;
        let peeled = obj
            .peel_to_kind(gix::object::Kind::Commit)
            .map_err(|e| err("peel_to_commit", e))?;
        Ok(Commit {
            repo: Arc::clone(&self.repo),
            id: peeled.id,
        })
    }

    /// Follow tag/commit objects until a tree is reached, returning it.
    #[napi]
    pub fn peel_to_tree(&self) -> napi::Result<Tree> {
        let r = thread_local(&self.repo);
        let obj = r.find_object(self.id).map_err(|e| err("find_object", e))?;
        let peeled = obj
            .peel_to_kind(gix::object::Kind::Tree)
            .map_err(|e| err("peel_to_tree", e))?;
        Ok(Tree {
            repo: Arc::clone(&self.repo),
            id: peeled.id,
        })
    }

    /// Follow tag/commit objects until a blob is reached, returning it.
    #[napi]
    pub fn peel_to_blob(&self) -> napi::Result<Blob> {
        let r = thread_local(&self.repo);
        let obj = r.find_object(self.id).map_err(|e| err("find_object", e))?;
        let peeled = obj
            .peel_to_kind(gix::object::Kind::Blob)
            .map_err(|e| err("peel_to_blob", e))?;
        Ok(build_blob(&self.repo, peeled.id))
    }

    /// Follow tag chains until a tag object is reached, returning it.
    #[napi]
    pub fn peel_to_tag(&self) -> napi::Result<Tag> {
        let r = thread_local(&self.repo);
        let obj = r.find_object(self.id).map_err(|e| err("find_object", e))?;
        let peeled = obj
            .peel_to_kind(gix::object::Kind::Tag)
            .map_err(|e| err("peel_to_tag", e))?;
        Ok(Tag {
            repo: Arc::clone(&self.repo),
            id: peeled.id,
        })
    }

    /// Follow tag/commit objects until an object of `kind` is reached, returning it.
    ///
    /// `kind` is `Commit`, `Tree`, `Blob`, or `Tag`; pass `Any` to follow tag chains to
    /// the first object that is *not* a tag (git's `<rev>^{}`, i.e. libgit2's
    /// `ObjectType::Any`) — see [`peel_to_end`](Self::peel_to_end).
    #[napi]
    pub fn peel_to_kind(&self, kind: ObjectType) -> napi::Result<Object> {
        let r = thread_local(&self.repo);
        let obj = r.find_object(self.id).map_err(|e| err("find_object", e))?;
        let peeled = match kind {
            ObjectType::Any => obj.peel_tags_to_end().map_err(|e| err("peel_to_kind", e))?,
            other => obj
                .peel_to_kind(to_object_kind(other)?)
                .map_err(|e| err("peel_to_kind", e))?,
        };
        build_object(&self.repo, peeled.id)
    }

    /// Follow tag chains until an object that is not a tag is reached, returning it.
    ///
    /// Unlike the other peel methods this never peels a commit to its tree: a tag on a
    /// commit yields that commit. This is `git rev-parse <rev>^{}`.
    #[napi]
    pub fn peel_to_end(&self) -> napi::Result<Object> {
        let r = thread_local(&self.repo);
        let obj = r.find_object(self.id).map_err(|e| err("find_object", e))?;
        let peeled = obj.peel_tags_to_end().map_err(|e| err("peel_to_end", e))?;
        build_object(&self.repo, peeled.id)
    }

    /// Follow tag/commit objects until an object of `kind` is reached, returning
    /// it. `Any` follows tag chains to the first object that is not a tag
    /// (`git rev-parse <rev>^{}`). This is a camelCase alias of
    /// [`peel_to_kind`](Self::peel_to_kind) for libgit2 parity.
    #[napi]
    pub fn peel(&self, kind: ObjectType) -> napi::Result<Object> {
        self.peel_to_kind(kind)
    }

    /// The shortest unambiguous abbreviation of this object's id.
    ///
    /// Starts at the `core.abbrev` length (7 by default) and extends until the prefix is
    /// unique, mirroring `git_object_short_id`. Falls back to the full id when it cannot be
    /// shortened (e.g. the object is missing), so this never throws for a valid handle.
    #[napi]
    pub fn short_id(&self) -> napi::Result<String> {
        let r = thread_local(&self.repo);
        let id = crate::replace::resolve_replace_head(&self.repo, &self.id).unwrap_or(self.id);
        Ok(short_sha(&r, &id))
    }

    /// Replace this object's content, recording a new revision in the versioned history.
    /// This is exactly the repository-level [`Repository::replace`] API, with this object's
    /// id supplied automatically.
    #[napi]
    pub fn replace(
        &self,
        content: Either<Buffer, String>,
        options: Option<ReplaceOptions>,
    ) -> napi::Result<ReplaceResult> {
        let repo = Repository {
            inner: Arc::clone(&self.repo),
        };
        repo.replace(self.id.to_string(), content, options)
    }

    // --- static constructors from the various object types ---

    /// Build an `Object` from a [`Blob`], resolving its content by id.
    #[napi]
    pub fn from_blob(blob: &Blob) -> napi::Result<Object> {
        build_object(&blob.repo, blob.id)
    }

    /// Build an `Object` from a [`Commit`], resolving its content by id.
    #[napi]
    pub fn from_commit(commit: &Commit) -> napi::Result<Object> {
        build_object(&commit.repo, commit.id)
    }

    /// Build an `Object` from a [`Tree`], resolving its content by id.
    #[napi]
    pub fn from_tree(tree: &Tree) -> napi::Result<Object> {
        build_object(&tree.repo, tree.id)
    }

    /// Build an `Object` from a [`Tag`], resolving its content by id.
    #[napi]
    pub fn from_tag(tag: &Tag) -> napi::Result<Object> {
        build_object(&tag.repo, tag.id)
    }

    /// Build an `Object` from an [`Entry`], resolving its target by id. Throws for a
    /// detached entry, which has no repository to resolve against.
    #[napi]
    pub fn from_entry(entry: &Entry) -> napi::Result<Object> {
        let repo = entry
            .repo
            .as_ref()
            .ok_or_else(|| err_str("Entry", "detached entry has no repository"))?;
        build_object(repo, entry.id)
    }
}

/// An object header: its kind, the size of its encoded data in bytes, and where it was found.
#[napi(object)]
pub struct ObjectHeader {
    /// The object kind (`"blob"`, `"tree"`, `"commit"`, or `"tag"`).
    pub kind: String,
    /// The size of the object's encoded data in bytes.
    pub size: f64,
    /// `true` if the object was located in a pack (vs. a loose object).
    pub is_packed: bool,
    /// Number of deltas decoded to obtain this header; `null` for loose objects, always present for packed ones.
    pub num_deltas: Option<f64>,
}

impl From<gix::odb::find::Header> for ObjectHeader {
    fn from(h: gix::odb::find::Header) -> Self {
        match h {
            gix::odb::find::Header::Loose { kind, size } => ObjectHeader {
                kind: kind_to_string(kind),
                size: size as f64,
                is_packed: false,
                num_deltas: None,
            },
            gix::odb::find::Header::Packed(out) => ObjectHeader {
                kind: kind_to_string(out.kind),
                size: out.object_size as f64,
                is_packed: true,
                num_deltas: Some(out.num_deltas as f64),
            },
        }
    }
}

/// An object's kind and its raw, decompressed body (without the loose-object header).
#[napi(object)]
pub struct ObjectData {
    pub kind: String,
    pub data: Buffer,
}

pub(crate) fn kind_to_string(kind: gix::object::Kind) -> String {
    match kind {
        gix::object::Kind::Commit => "commit",
        gix::object::Kind::Tree => "tree",
        gix::object::Kind::Blob => "blob",
        gix::object::Kind::Tag => "tag",
    }
    .to_string()
}

/// Parse an object hash kind name, accepting any casing (`sha1`/`Sha1`, `sha256`/`Sha256`).
///
/// Shared so that every entry point taking a hash kind accepts the same spellings —
/// [`Repository::object_hash`] emits the `Debug` casing (`"Sha1"`), which must round-trip.
pub(crate) fn parse_hash_kind_ci(s: &str) -> Option<gix::hash::Kind> {
    match s.to_lowercase().as_str() {
        "sha1" => Some(gix::hash::Kind::Sha1),
        "sha256" => Some(gix::hash::Kind::Sha256),
        _ => None,
    }
}

/// Resolve an optional object-format name to a [`gix::hash::Kind`], defaulting to SHA-1
/// (libgit2's default for the zero / empty-object ids). Used by the `Oid` factories so
/// that `"sha256"` selects the SHA-256 empty/zero id.
pub(crate) fn oid_kind_or_default(format: Option<&str>) -> napi::Result<gix::hash::Kind> {
    match format {
        None => Ok(gix::hash::Kind::Sha1),
        Some(s) => parse_hash_kind_ci(s)
            .ok_or_else(|| err_str("Oid", format!("unknown object hash kind {s:?}"))),
    }
}

pub(crate) fn string_to_kind(s: &str) -> napi::Result<gix::object::Kind> {
    match s.to_lowercase().as_str() {
        "commit" => Ok(gix::object::Kind::Commit),
        "tree" => Ok(gix::object::Kind::Tree),
        "blob" => Ok(gix::object::Kind::Blob),
        "tag" => Ok(gix::object::Kind::Tag),
        other => Err(err_str(
            "write_object",
            format!("invalid object kind {other}"),
        )),
    }
}

/// A git object id (SHA-1 or SHA-256 digest) exposed to TypeScript.
///
/// A convenience wrapper over the hex strings the rest of the API uses: repository
/// methods still take hex ids, so pass `oid.hex` (or `String(oid)`) wherever an id is
/// expected.
///
/// ```ts
/// const oid = new Oid(repo.headId());
/// oid.hex; // 40- or 64-char hex
/// oid.format(); // "sha1" | "sha256"
/// oid.isZero(); // false
/// Oid.fromBytes(oid.toBytes()).equals(oid); // true
/// ```
#[napi]
pub struct Oid {
    inner: gix::hash::ObjectId,
}

#[napi]
impl Oid {
    /// Parse a hex object id: 40 hex characters for SHA-1 or 64 for SHA-256. Throws on
    /// empty, wrongly-sized, or non-hex input — abbreviated ids are not object ids.
    #[napi(constructor)]
    pub fn new(hex: String) -> napi::Result<Self> {
        let inner = gix::hash::ObjectId::from_hex(hex.as_bytes()).map_err(|e| err("Oid", e))?;
        Ok(Self { inner })
    }

    /// Parse a hex object id, optionally validating that its length matches `format`
    /// (`"sha1"` or `"sha256"`). Without `format` this is identical to the
    /// [`new`](Self::new) constructor (auto-detecting the hash kind by digest length);
    /// with it, a string whose length does not match the requested format is rejected
    /// — mirroring libgit2's `git_oid__fromstr_ext` strict behaviour.
    #[napi(factory)]
    pub fn from_hex(hex: String, format: Option<String>) -> napi::Result<Self> {
        if let Some(format) = format.as_deref() {
            let Some(kind) = parse_hash_kind_ci(format) else {
                return Err(err_str(
                    "Oid.from_hex",
                    format!("unknown object hash kind {format:?}"),
                ));
            };
            if hex.len() != kind.len_in_hex() {
                return Err(err_str(
                    "Oid.from_hex",
                    format!(
                        "object id length {} does not match {format} format (expected {})",
                        hex.len(),
                        kind.len_in_hex()
                    ),
                ));
            }
        }
        let inner = gix::hash::ObjectId::from_hex(hex.as_bytes()).map_err(|e| err("Oid", e))?;
        Ok(Self { inner })
    }

    /// The lowercase hex representation of this object id.
    #[napi(getter)]
    pub fn hex(&self) -> String {
        self.inner.to_string()
    }

    /// The hex representation (so `String(oid)` and template literals work).
    #[napi]
    pub fn to_string(&self) -> String {
        self.inner.to_string()
    }

    /// The hex representation (so `JSON.stringify(oid)` yields the id).
    #[napi(js_name = "toJSON")]
    pub fn to_json(&self) -> String {
        self.inner.to_string()
    }

    /// The object format of this id: `"sha1"` (20-byte digest) or `"sha256"` (32-byte).
    #[napi]
    pub fn format(&self) -> String {
        match self.inner.kind() {
            gix::hash::Kind::Sha1 => "sha1",
            // `gix::hash::Kind` is `#[non_exhaustive]`; any other (future) kind
            // is still a 32-byte-or-larger digest and is reported as `sha256`.
            _ => "sha256",
        }
        .to_string()
    }

    /// Whether this id is the all-zero id (libgit2's `git_oid_is_zero`).
    #[napi]
    pub fn is_zero(&self) -> bool {
        self.inner.is_null()
    }

    /// The raw digest bytes of this id (20 for SHA-1, 32 for SHA-256).
    #[napi]
    pub fn to_bytes(&self) -> Buffer {
        Buffer::from(self.inner.as_bytes().to_vec())
    }

    /// Whether `other` holds the same digest as this id.
    #[napi]
    pub fn equals(&self, other: &Oid) -> bool {
        self.inner == other.inner
    }

    /// Compare this id with `other` byte-wise: `-1`, `0`, or `1` (libgit2's `git_oid_cmp`).
    #[napi]
    pub fn compare(&self, other: &Oid) -> i32 {
        match self.inner.cmp(&other.inner) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        }
    }

    /// Build an id from raw digest bytes (20 for SHA-1, 32 for SHA-256). Throws for any
    /// other length.
    #[napi(factory)]
    pub fn from_bytes(bytes: Buffer) -> napi::Result<Self> {
        let inner =
            gix::hash::ObjectId::try_from(&bytes[..]).map_err(|e| err("Oid.from_bytes", e))?;
        Ok(Self { inner })
    }

    /// The all-zero id for `format` (`"sha1"` by default, or `"sha256"`) — libgit2's
    /// `Oid::ZERO_SHA1` / `Oid::ZERO_SHA256`.
    #[napi(factory)]
    pub fn zero(format: Option<String>) -> napi::Result<Self> {
        let kind = oid_kind_or_default(format.as_deref())?;
        Ok(Self {
            inner: gix::hash::ObjectId::null(kind),
        })
    }

    /// The id of an empty blob for `format` (`"sha1"` by default, or `"sha256"`).
    /// SHA-256 repositories store the 32-byte empty-blob id, so this is the
    /// SHA-256-aware counterpart of libgit2's empty-blob id.
    #[napi(factory)]
    pub fn empty_blob(format: Option<String>) -> napi::Result<Self> {
        let kind = oid_kind_or_default(format.as_deref())?;
        Ok(Self {
            inner: gix::hash::ObjectId::empty_blob(kind),
        })
    }

    /// The id of an empty tree for `format` (`"sha1"` by default, or `"sha256"`).
    /// SHA-256 repositories store the 32-byte empty-tree id, so this is the
    /// SHA-256-aware counterpart of libgit2's empty-tree id.
    #[napi(factory)]
    pub fn empty_tree(format: Option<String>) -> napi::Result<Self> {
        let kind = oid_kind_or_default(format.as_deref())?;
        Ok(Self {
            inner: gix::hash::ObjectId::empty_tree(kind),
        })
    }
}

/// Whether `id` is a valid object id: lowercase hex of the full digest length.
///
/// Without `format`, both 40-hex (SHA-1) and 64-hex (SHA-256) ids are accepted; with it,
/// the length must match (`oidIsValid(id, "sha256")`). Abbreviated ids are *not* valid —
/// resolve those with `revParseSingle`, `ObjectDatabase.disambiguatePrefix`, or shorten
/// with `Object.shortId()`.
#[napi]
pub fn oid_is_valid(id: String, format: Option<String>) -> bool {
    if let Some(format) = format.as_deref() {
        let Some(kind) = parse_hash_kind_ci(format) else {
            return false;
        };
        if id.len() != kind.len_in_hex() {
            return false;
        }
    }
    gix::hash::ObjectId::from_hex(id.as_bytes()).is_ok()
}

/// The object format of `id` (`"sha1"` or `"sha256"`), or `null` when it is not a valid
/// object id. This mirrors libgit2's `git_oid_object_format` without needing a repository.
#[napi]
pub fn oid_format(id: String) -> Option<String> {
    let oid = gix::hash::ObjectId::from_hex(id.as_bytes()).ok()?;
    Some(
        match oid.kind() {
            gix::hash::Kind::Sha1 => "sha1",
            _ => "sha256",
        }
        .to_string(),
    )
}

/// Whether `id` is the all-zero object id (libgit2's `git_oid_is_zero`).
///
/// `false` for strings that are not valid object ids.
#[napi]
pub fn oid_is_zero(id: String) -> bool {
    gix::hash::ObjectId::from_hex(id.as_bytes())
        .map(|oid| oid.is_null())
        .unwrap_or(false)
}

/// The all-zero object id: `0` repeated 40 times for `sha1` (the default) or 64 times for
/// `sha256`. This is libgit2's `Oid::ZERO_SHA1` / `Oid::ZERO_SHA256`.
#[napi]
pub fn oid_zero(format: Option<String>) -> napi::Result<String> {
    let kind = oid_kind_or_default(format.as_deref())?;
    Ok(gix::hash::ObjectId::null(kind).to_string())
}

/// The raw digest bytes of `id` (20 bytes for SHA-1, 32 for SHA-256). Throws when `id` is
/// not a valid object id.
#[napi]
pub fn oid_to_bytes(id: String) -> napi::Result<Buffer> {
    let oid = gix::hash::ObjectId::from_hex(id.as_bytes()).map_err(|e| err("oid_to_bytes", e))?;
    Ok(Buffer::from(oid.as_bytes().to_vec()))
}

/// The hex object id of a raw digest (`oid_to_bytes`' inverse). Throws unless `bytes` is
/// exactly 20 (SHA-1) or 32 (SHA-256) bytes long.
#[napi]
pub fn oid_from_bytes(bytes: Buffer) -> napi::Result<String> {
    let oid = gix::hash::ObjectId::try_from(&bytes[..]).map_err(|e| err("oid_from_bytes", e))?;
    Ok(oid.to_string())
}

#[napi]
impl Repository {
    /// Find an object of any kind, returning a wrapper holding its decoded content.
    #[napi]
    pub fn find_object(&self, id: String) -> napi::Result<Object> {
        let oid = parse_oid(&id)?;
        let r = thread_local(&self.inner);
        let obj = r.find_object(oid).map_err(|e| err("find_object", e))?;
        let inner = decode_object(obj, r.object_hash()).map_err(|e| err("decode", e))?;
        Ok(Object {
            repo: Arc::clone(&self.inner),
            id: oid,
            inner,
        })
    }

    /// Whether an object with the given id exists.
    #[napi]
    pub fn object_exists(&self, id: String) -> bool {
        let r = thread_local(&self.inner);
        match parse_oid(&id) {
            Ok(oid) => r.has_object(oid),
            Err(_) => false,
        }
    }

    /// Peel an object to a target object kind (`Commit`, `Tree`, `Blob`, `Tag`).
    #[napi]
    pub fn peel(&self, id: String, kind: ObjectType) -> napi::Result<String> {
        let r = thread_local(&self.inner);
        let oid = parse_oid(&id)?;
        let target_kind = match kind {
            ObjectType::Commit => gix::objs::Kind::Commit,
            ObjectType::Tree => gix::objs::Kind::Tree,
            ObjectType::Blob => gix::objs::Kind::Blob,
            ObjectType::Tag => gix::objs::Kind::Tag,
            ObjectType::Any => {
                return Err(err_str(
                    "peel",
                    "\"Any\" is not supported here; use Object.peelToKind",
                ));
            }
        };
        let obj = r.find_object(oid).map_err(|e| err("find_object", e))?;
        let peeled = obj.peel_to_kind(target_kind).map_err(|e| err("peel", e))?;
        Ok(peeled.id().to_string())
    }

    /// Peel an object to a target object kind (`Commit`, `Tree`, `Blob`, `Tag`)
    /// and return the resulting [`Object`] directly (rather than just its id),
    /// avoiding a follow-up [`find_object`](Self::find_object) call.
    #[napi]
    pub fn peel_to_object(&self, id: String, kind: ObjectType) -> napi::Result<Object> {
        let r = thread_local(&self.inner);
        let oid = parse_oid(&id)?;
        let target_kind = match kind {
            ObjectType::Commit => gix::objs::Kind::Commit,
            ObjectType::Tree => gix::objs::Kind::Tree,
            ObjectType::Blob => gix::objs::Kind::Blob,
            ObjectType::Tag => gix::objs::Kind::Tag,
            ObjectType::Any => {
                return Err(err_str(
                    "peel_to_object",
                    "\"Any\" is not supported here; use Object.peel",
                ));
            }
        };
        let obj = r.find_object(oid).map_err(|e| err("find_object", e))?;
        let peeled = obj.peel_to_kind(target_kind).map_err(|e| err("peel", e))?;
        build_object(&self.inner, peeled.id)
    }

    /// Try to find an object by id, returning null if missing instead of throwing.
    #[napi]
    pub fn try_find_object(&self, id: String) -> napi::Result<Option<Object>> {
        let r = thread_local(&self.inner);
        match parse_oid(&id) {
            Ok(oid) => match r.find_object(oid) {
                Ok(obj) => Ok(Some(Object {
                    repo: Arc::clone(&self.inner),
                    id: oid,
                    inner: decode_object(obj, r.object_hash()).map_err(|e| err("decode", e))?,
                })),
                Err(_) => Ok(None),
            },
            Err(_) => Ok(None),
        }
    }

    /// Write a raw object of the given `kind` (`Commit`, `Tree`, `Blob`, `Tag`) and return its id.
    #[napi]
    pub fn write_object(&self, kind: ObjectType, data: Buffer) -> napi::Result<String> {
        let r = thread_local(&self.inner);
        let kind = to_object_kind(kind)?;
        let data = data.to_vec();
        let mut buf = data.as_slice();
        let id = r
            .write_stream(kind, data.len() as u64, &mut buf)
            .map_err(|e| err("write_object", e))?;
        Ok(id.to_string())
    }

    /// Read an object's kind and raw, decompressed body (the inverse of [`write_object`](Self::write_object)).
    #[napi]
    pub fn read_object(&self, id: String) -> napi::Result<ObjectData> {
        let r = thread_local(&self.inner);
        let oid = parse_oid(&id)?;
        let resolved = crate::replace::resolve_replace_head(&self.inner, &oid).unwrap_or(oid);
        let obj = r
            .find_object(resolved)
            .map_err(|e| err("find_object", e))?
            .detach();
        Ok(ObjectData {
            kind: kind_to_string(obj.kind),
            data: Buffer::from(obj.data),
        })
    }

    /// Compute the object id the file at `path` would have as an object of `kind`
    /// (`Commit`, `Tree`, `Blob`, `Tag`), using this repository's hash kind.
    ///
    /// Nothing is written to the object database — this is libgit2's `Oid::hash_file`.
    /// Throws when the file cannot be opened or read.
    #[napi]
    pub fn compute_hash_file(&self, kind: ObjectType, path: String) -> napi::Result<String> {
        let r = thread_local(&self.inner);
        let kind = to_object_kind(kind)?;
        let mut file = std::fs::File::open(&path).map_err(|e| err("open", e))?;
        let len = file.metadata().map_err(|e| err("metadata", e))?.len();
        let should_interrupt = std::sync::atomic::AtomicBool::new(false);
        let id = gix::objs::compute_stream_hash(
            r.object_hash(),
            kind,
            &mut file,
            len,
            &mut gix::progress::Discard,
            &should_interrupt,
        )
        .map_err(|e| err("compute_hash_file", e))?;
        Ok(id.to_string())
    }

    /// Compute the object id of `data` of the given `kind` (`Commit`, `Tree`, `Blob`, `Tag`)
    /// without writing it to the object database.
    #[napi]
    pub fn compute_hash(&self, kind: ObjectType, data: Buffer) -> napi::Result<String> {
        let r = thread_local(&self.inner);
        let kind = to_object_kind(kind)?;
        let id = gix::objs::compute_hash(r.object_hash(), kind, &data)
            .map_err(|e| err("compute_hash", e))?;
        Ok(id.to_string())
    }

    /// Read an object's header (kind + size) without decoding its full content.
    #[napi]
    pub fn find_header(&self, id: String) -> napi::Result<ObjectHeader> {
        let r = thread_local(&self.inner);
        let header = r
            .find_header(parse_oid(&id)?)
            .map_err(|e| err("find_header", e))?;
        Ok(ObjectHeader::from(header))
    }

    /// Like [`find_header`](Self::find_header) but returns `null` if the object is missing.
    #[napi]
    pub fn try_find_header(&self, id: String) -> napi::Result<Option<ObjectHeader>> {
        let r = thread_local(&self.inner);
        match parse_oid(&id) {
            Ok(oid) => match r.find_header(oid) {
                Ok(header) => Ok(Some(ObjectHeader::from(header))),
                Err(_) => Ok(None),
            },
            Err(_) => Ok(None),
        }
    }

    /// Set the in-memory object cache size (in bytes). Pass `0` to disable.
    ///
    /// Note: each synchronous call gets a fresh thread-local repository view, so this only
    /// affects the cache of the view used within this single call.
    #[napi]
    pub fn object_cache_size(&self, bytes: f64) -> napi::Result<()> {
        let mut r = thread_local(&self.inner);
        r.object_cache_size(bytes as usize);
        Ok(())
    }

    /// Set the object cache size only if it has not been set already.
    ///
    /// See [`object_cache_size`](Self::object_cache_size) for the scoping caveat.
    #[napi]
    pub fn object_cache_size_if_unset(&self, bytes: f64) -> napi::Result<()> {
        let mut r = thread_local(&self.inner);
        r.object_cache_size_if_unset(bytes as usize);
        Ok(())
    }

    /// Compute a recommended object cache size (in bytes) for tree-diff operations,
    /// based on the number of tracked files in the index.
    #[napi]
    pub fn compute_object_cache_size_for_tree_diffs(&self) -> napi::Result<f64> {
        let r = thread_local(&self.inner);
        let index = r.index().map_err(|e| err("index", e))?;
        Ok(r.compute_object_cache_size_for_tree_diffs(&*index) as f64)
    }

    /// Enable keeping decompressed objects in memory for the lifetime of this view.
    ///
    /// See [`object_cache_size`](Self::object_cache_size) for the scoping caveat.
    #[napi]
    pub fn with_object_memory(&self) -> napi::Result<()> {
        let r = thread_local(&self.inner);
        r.with_object_memory();
        Ok(())
    }
}
