//! SQLite backend for git object store.
//!
//! Stores git objects in a SQLite database (file-backed or `:memory:`).
//! Implements the shared [`ObjectStore`] base trait, so its napi surface matches
//! [`crate::object_store::LooseObjectStore`] and [`crate::object_store::MemoryObjectStore`].

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use diesel::connection::SimpleConnection;
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;
use gix::ObjectId;
use gix::object::Kind;
use gix::zlib::Compression;
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use crate::error::{err, err_str, parse_oid};
use crate::object::{kind_to_string, string_to_kind};
use crate::object_store::base::ObjectStore;
use crate::object_store::{
    ObjectStoreReader, ObjectStoreWriter, PrefixLookup, WriterTarget, object_kind_from_option,
};

diesel::table! {
    git_objects (oid) {
        id -> Integer,
        oid -> Text,
        kind -> Text,
        size -> BigInt,
        data -> Binary,
        last_modified -> BigInt,
    }
}

#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = git_objects)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct StoredGitObject {
    id: i32,
    oid: String,
    kind: String,
    size: i64,
    data: Vec<u8>,
    last_modified: i64,
}

#[derive(Insertable)]
#[diesel(table_name = git_objects)]
struct NewGitObject<'a> {
    oid: &'a str,
    kind: &'a str,
    size: i64,
    data: &'a [u8],
    last_modified: i64,
}

pub(crate) struct SqlStore {
    conn: Mutex<SqliteConnection>,
    kind: gix::hash::Kind,
    url: String,
}

impl SqlStore {
    pub(crate) fn new(database_url: &str, kind: gix::hash::Kind) -> napi::Result<Self> {
        let mut conn = SqliteConnection::establish(database_url)
            .map_err(|e| err("SqlObjectStore::new", e))?;

        // Optimize SQLite pragmas for git object store usage
        conn.batch_execute(
            r#"
            PRAGMA foreign_keys = ON;
            PRAGMA busy_timeout = 5000;
            CREATE TABLE IF NOT EXISTS git_objects (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                oid TEXT UNIQUE NOT NULL,
                kind TEXT NOT NULL,
                size INTEGER NOT NULL,
                data BLOB NOT NULL,
                last_modified INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_git_objects_oid ON git_objects(oid);
            CREATE INDEX IF NOT EXISTS idx_git_objects_id ON git_objects(id);
            "#,
        )
        .map_err(|e| err("SqlObjectStore::new", e))?;

        Ok(Self {
            conn: Mutex::new(conn),
            kind,
            url: database_url.to_string(),
        })
    }

    fn insert_entry(&self, id: ObjectId, kind: Kind, data: &[u8]) -> napi::Result<()> {
        use git_objects::dsl as go;

        let oid_str = id.to_string();
        let kind_str = kind_to_string(kind);
        let now_millis = Utc::now().timestamp_millis();
        let size = data.len() as i64;

        let mut conn = self.conn.lock().unwrap();
        diesel::insert_into(go::git_objects)
            .values((
                go::oid.eq(&oid_str),
                go::kind.eq(&kind_str),
                go::size.eq(size),
                go::data.eq(data),
                go::last_modified.eq(now_millis),
            ))
            .on_conflict(go::oid)
            .do_update()
            .set((
                go::kind.eq(&kind_str),
                go::size.eq(size),
                go::data.eq(data),
                go::last_modified.eq(now_millis),
            ))
            .execute(&mut *conn)
            .map_err(|e| err("SqlObjectStore::insert", e))?;

        Ok(())
    }

    fn contains(&self, id: &str) -> bool {
        use git_objects::dsl as go;
        let mut conn = self.conn.lock().unwrap();
        diesel::select(diesel::dsl::exists(go::git_objects.filter(go::oid.eq(id))))
            .get_result(&mut *conn)
            .unwrap_or(false)
    }

    fn try_find(&self, id: &str) -> napi::Result<Option<crate::object::ObjectData>> {
        use git_objects::dsl as go;
        let mut conn = self.conn.lock().unwrap();
        let res: Option<(String, Vec<u8>)> = go::git_objects
            .filter(go::oid.eq(id))
            .select((go::kind, go::data))
            .first(&mut *conn)
            .optional()
            .map_err(|e| err("SqlObjectStore::try_find", e))?;

        Ok(res.map(|(k, data)| crate::object::ObjectData {
            kind: k,
            data: Buffer::from(data),
        }))
    }

    fn try_header(&self, id: &str) -> napi::Result<Option<crate::object::ObjectHeader>> {
        use git_objects::dsl as go;
        let mut conn = self.conn.lock().unwrap();
        let res: Option<(String, i64)> = go::git_objects
            .filter(go::oid.eq(id))
            .select((go::kind, go::size))
            .first(&mut *conn)
            .optional()
            .map_err(|e| err("SqlObjectStore::try_header", e))?;

        Ok(res.map(|(k, sz)| crate::object::ObjectHeader {
            kind: k,
            size: sz as f64,
            is_packed: false,
            num_deltas: None,
        }))
    }

    fn lookup_prefix(&self, prefix: &str) -> napi::Result<PrefixLookup> {
        use git_objects::dsl as go;
        let p = prefix.to_ascii_lowercase();
        let pattern = format!("{p}%");

        let mut conn = self.conn.lock().unwrap();
        let matches: Vec<String> = go::git_objects
            .filter(go::oid.like(pattern))
            .select(go::oid)
            .limit(2)
            .load(&mut *conn)
            .map_err(|e| err("SqlObjectStore::lookup_prefix", e))?;

        Ok(match matches.len() {
            1 => PrefixLookup {
                found: true,
                ambiguous: false,
                id: Some(matches.into_iter().next().unwrap()),
            },
            n if n > 1 => PrefixLookup {
                found: false,
                ambiguous: true,
                id: None,
            },
            _ => PrefixLookup {
                found: false,
                ambiguous: false,
                id: None,
            },
        })
    }

    fn reader(&self, id: &str) -> napi::Result<ObjectStoreReader> {
        let obj = self
            .try_find(id)?
            .ok_or_else(|| err_str("reader", format!("object {id} not found")))?;
        Ok(ObjectStoreReader {
            data: obj.data.to_vec(),
            kind: obj.kind,
            pos: 0,
        })
    }

    pub(crate) fn write_object(&self, kind: Kind, data: &[u8]) -> napi::Result<ObjectId> {
        let mut h = gix::hash::hasher(self.kind);
        let header = format!("{} {}\0", kind_to_string(kind), data.len());
        h.update(header.as_bytes());
        h.update(data);
        let id = h.try_finalize().map_err(|e| err("write_object", e))?;
        self.insert_entry(id, kind, data)?;
        Ok(id)
    }

    fn write_object_with_id(
        &self,
        id: ObjectId,
        kind: Kind,
        data: &[u8],
    ) -> napi::Result<ObjectId> {
        self.insert_entry(id, kind, data)?;
        Ok(id)
    }

    fn insert_objects(&self, objects: Vec<(ObjectId, (Kind, Vec<u8>))>) -> napi::Result<()> {
        for (id, (kind, data)) in objects {
            self.insert_entry(id, kind, &data)?;
        }
        Ok(())
    }

    fn all_entries(&self) -> napi::Result<Vec<(ObjectId, Kind, Vec<u8>)>> {
        use git_objects::dsl as go;
        let mut conn = self.conn.lock().unwrap();
        let rows: Vec<(String, String, Vec<u8>)> = go::git_objects
            .order(go::id.asc())
            .select((go::oid, go::kind, go::data))
            .load(&mut *conn)
            .map_err(|e| err("SqlObjectStore::all_entries", e))?;

        let mut out = Vec::with_capacity(rows.len());
        for (oid_str, kind_str, data) in rows {
            let oid = parse_oid(&oid_str)?;
            let kind = string_to_kind(&kind_str)?;
            out.push((oid, kind, data));
        }
        Ok(out)
    }

    fn entries(&self) -> napi::Result<Vec<String>> {
        use git_objects::dsl as go;
        let mut conn = self.conn.lock().unwrap();
        go::git_objects
            .order(go::id.asc())
            .select(go::oid)
            .load(&mut *conn)
            .map_err(|e| err("SqlObjectStore::entries", e))
    }

    fn size(&self) -> napi::Result<f64> {
        use git_objects::dsl as go;
        let mut conn = self.conn.lock().unwrap();
        let count: i64 = go::git_objects
            .count()
            .get_result(&mut *conn)
            .map_err(|e| err("SqlObjectStore::size", e))?;
        Ok(count as f64)
    }

    fn last_modified(&self, id: &str) -> napi::Result<Option<DateTime<Utc>>> {
        use git_objects::dsl as go;
        let mut conn = self.conn.lock().unwrap();
        let millis: Option<i64> = go::git_objects
            .filter(go::oid.eq(id))
            .select(go::last_modified)
            .first(&mut *conn)
            .optional()
            .map_err(|e| err("SqlObjectStore::last_modified", e))?;

        Ok(millis.and_then(DateTime::from_timestamp_millis))
    }

    fn reset(&self) -> napi::Result<()> {
        use git_objects::dsl as go;
        let mut conn = self.conn.lock().unwrap();
        diesel::delete(go::git_objects)
            .execute(&mut *conn)
            .map_err(|e| err("SqlObjectStore::reset", e))?;
        Ok(())
    }
}

/// A git object store backed by a SQLite database.
///
/// Objects are persisted in a `git_objects` table in SQLite, providing full persistence,
/// transactional updates, and SQL queryability while implementing the [`ObjectStore`] trait.
#[napi]
#[derive(Clone)]
pub struct SqlObjectStore {
    inner: Arc<SqlStore>,
}

impl ObjectStore for SqlObjectStore {
    fn kind(&self) -> gix::hash::Kind {
        self.inner.kind
    }

    fn contains_id(&self, id: &str) -> bool {
        self.inner.contains(id)
    }

    fn lookup_prefix_impl(&self, prefix: &str) -> napi::Result<PrefixLookup> {
        self.inner.lookup_prefix(prefix)
    }

    fn find_impl(&self, id: &str) -> napi::Result<Option<crate::object::ObjectData>> {
        self.inner.try_find(id)
    }

    fn header_impl(&self, id: &str) -> napi::Result<Option<crate::object::ObjectHeader>> {
        self.inner.try_header(id)
    }

    fn write_buf_impl(&self, kind: Kind, data: &[u8]) -> napi::Result<ObjectId> {
        self.inner.write_object(kind, data)
    }

    fn write_buf_with_id_impl(
        &self,
        kind: Kind,
        data: &[u8],
        id: ObjectId,
    ) -> napi::Result<ObjectId> {
        self.inner.write_object_with_id(id, kind, data)
    }

    fn object_path_impl(&self, _id: &str) -> napi::Result<String> {
        Err(err_str(
            "object_path",
            "sqlite-backed store has no loose-object file path",
        ))
    }

    fn store_path(&self) -> String {
        self.inner.url.clone()
    }

    fn write_target(&self) -> WriterTarget {
        WriterTarget::Sql(self.inner.clone())
    }

    fn read_data_impl(&self, id: &str) -> napi::Result<ObjectStoreReader> {
        self.inner.reader(id)
    }

    fn insert_objects(&self, objects: Vec<(ObjectId, (Kind, Vec<u8>))>) -> napi::Result<()> {
        self.inner.insert_objects(objects)
    }
}

#[napi]
impl SqlObjectStore {
    /// Open or create a SQLite-backed git object store.
    ///
    /// `database_url` can be a file path (e.g. `objects.db`, `/path/to/repo.sqlite`) or `":memory:"`.
    /// `object_hash` is `"sha1"` (default) or `"sha256"`.
    #[napi(constructor)]
    pub fn new(database_url: String, object_hash: Option<String>) -> napi::Result<Self> {
        let kind = object_kind_from_option(object_hash.as_deref())?;
        let inner = SqlStore::new(&database_url, kind)?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Whether an object with the given id exists.
    #[napi]
    pub fn contains(&self, id: String) -> bool {
        ObjectStore::contains(self, id)
    }

    /// Resolve a short object id `prefix` (hex) to a unique object.
    #[napi]
    pub fn lookup_prefix(&self, prefix: String) -> napi::Result<PrefixLookup> {
        ObjectStore::lookup_prefix(self, prefix)
    }

    /// Read an object's raw, decompressed body and kind, or `null` if missing.
    #[napi]
    pub fn try_find(&self, id: String) -> napi::Result<Option<crate::object::ObjectData>> {
        ObjectStore::try_find(self, id)
    }

    /// Read an object's header (kind + size) without decoding its full content, or `null`.
    #[napi]
    pub fn try_header(&self, id: String) -> napi::Result<Option<crate::object::ObjectHeader>> {
        ObjectStore::try_header(self, id)
    }

    /// Write a raw object of the given `kind` (`commit`, `tree`, `blob`, `tag`) and return its id.
    #[napi]
    pub fn write_object(&self, kind: String, data: Buffer) -> napi::Result<String> {
        ObjectStore::write_object(self, kind, data)
    }

    /// SQLite-backed stores have no loose-object file path; this always errors.
    #[napi]
    pub fn object_path(&self, _id: String) -> napi::Result<String> {
        ObjectStore::object_path(self, _id)
    }

    /// The database URL / path this store is connected to.
    #[napi(getter)]
    pub fn path(&self) -> String {
        ObjectStore::path(self)
    }

    /// Return the hash kind this store uses (`"sha1"` or `"sha256"`).
    #[napi(getter)]
    pub fn object_hash(&self) -> String {
        ObjectStore::object_hash(self)
    }

    /// The object format this store uses, lower-cased (`"sha1"` or `"sha256"`).
    #[napi(getter)]
    pub fn object_format(&self) -> String {
        ObjectStore::object_format(self)
    }

    /// Open a streaming reader for the object identified by `id`.
    #[napi]
    pub fn reader(&self, id: String) -> napi::Result<ObjectStoreReader> {
        ObjectStore::reader(self, id)
    }

    /// Open a streaming writer for a new object of the given `kind` and total `size` in bytes.
    #[napi]
    pub fn writer(&self, kind: String, size: f64) -> napi::Result<ObjectStoreWriter> {
        ObjectStore::writer(self, kind, size)
    }

    /// Write a raw object of the given `kind` under the exact `id`, without recomputing its hash.
    #[napi]
    pub fn write_object_with_id(
        &self,
        kind: String,
        data: Buffer,
        id: String,
    ) -> napi::Result<String> {
        ObjectStore::write_object_with_id(self, kind, data, id)
    }

    /// Ingest a packfile `Buffer` into the store, adding all of its objects.
    #[napi]
    pub fn import_stream(&self, pack: Buffer) -> napi::Result<()> {
        ObjectStore::import_stream(self, pack)
    }

    /// Serialise every object currently held in the database into a git packfile `Buffer`.
    #[napi]
    pub fn dump(&self) -> napi::Result<Buffer> {
        let kind = self.inner.kind;
        let entries = self.inner.all_entries()?;
        let mut pack_entries = Vec::new();
        for (id, k, data) in &entries {
            let count = gix_pack::data::output::Count {
                id: *id,
                entry_pack_location: gix_pack::data::output::count::PackLocation::NotLookedUp,
            };
            let obj = gix::objs::Data {
                kind: *k,
                object_hash: kind,
                data: data.as_slice(),
            };
            let entry =
                gix_pack::data::output::Entry::from_data(&count, &obj, Compression::DEFAULT)
                    .map_err(|e| err("dump", e))?;
            pack_entries.push(entry);
        }
        let num = pack_entries.len() as u32;
        let mut out = Vec::new();
        let iter = std::iter::once(Ok(pack_entries));
        let mut writer = gix_pack::data::output::bytes::FromEntriesIter::new(
            iter,
            &mut out,
            num,
            gix_pack::data::Version::V2,
            kind,
        );
        while let Some(res) = writer.next() {
            res.map_err(|e| err("dump", e))?;
        }
        Ok(Buffer::from(out))
    }

    /// Load a packfile `Buffer` into the store, adding all of its objects.
    #[napi]
    pub fn load(&self, pack: Buffer) -> napi::Result<()> {
        ObjectStore::import_stream(self, pack)
    }

    /// Remove every object currently held in the database, leaving it empty.
    #[napi]
    pub fn reset(&self) -> napi::Result<()> {
        self.inner.reset()
    }

    /// Return the hex ids of every object, in insertion order (`id ASC`).
    #[napi]
    pub fn entries(&self) -> napi::Result<Vec<String>> {
        self.inner.entries()
    }

    /// Return the number of objects currently held in the database.
    #[napi]
    pub fn size(&self) -> napi::Result<f64> {
        self.inner.size()
    }

    /// Return the last-modified time (milliseconds since the Unix epoch) of the
    /// object identified by `id`, or `null` if it is not present.
    #[napi]
    pub fn last_modified(&self, id: String) -> napi::Result<Option<f64>> {
        Ok(self
            .inner
            .last_modified(&id)?
            .map(|t| t.timestamp_millis() as f64))
    }

    /// Copy the object identified by `from_id` to a new object stored under `to_id`
    /// (content stored under the new key), returning `to_id`.
    #[napi]
    pub fn copy(&self, from_id: String, to_id: String) -> napi::Result<String> {
        let from = parse_oid(&from_id)?;
        let to = parse_oid(&to_id)?;
        let (kind, data) = {
            let found = self
                .inner
                .try_find(&from.to_string())?
                .ok_or_else(|| err_str("copy", format!("object {from_id} not found")))?;
            (string_to_kind(&found.kind)?, found.data.to_vec())
        };
        self.inner.write_object_with_id(to, kind, &data)?;
        Ok(to_id)
    }
}
