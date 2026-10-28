use crate::error::RepositoryResult as Result;
use napi_derive::napi;

use crate::error::{err, err_str, thread_local};
use crate::repository::Repository;

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// A single configuration key/value entry.
#[derive(Clone)]
#[napi(object)]
pub struct ConfigEntry {
    /// The fully-qualified key, e.g. `user.name`.
    pub key: String,
    /// The value as a UTF-8 string.
    pub value: String,
    /// The configuration source (precedence category) this entry was read from, e.g.
    /// `Local`, `User`, `System`; `null` when not determined.
    pub level: Option<String>,
    /// How many `include` levels deep this entry was loaded from (`0` = directly loaded).
    pub include_depth: Option<u32>,
}

/// Map a gix configuration `Source` to the same variant names used by `ConfigSource`.
fn source_name(s: gix::config::Source) -> String {
    use gix::config::Source::*;
    match s {
        GitInstallation => "GitInstallation",
        System => "System",
        Git => "Git",
        User => "User",
        Local => "Local",
        Worktree => "Worktree",
        Env => "Env",
        Cli => "Cli",
        Api => "Api",
        EnvOverride => "EnvOverride",
    }
    .to_string()
}

/// Simplified fnmatch for `entries(glob)`: matches when the key equals the pattern or,
/// when the pattern contains `*`, when each literal segment appears in order (a trailing
/// `*` is implicit). Case-sensitive, matching git's normalized variable matching.
fn key_glob_match(key: &[u8], glob: &[u8]) -> bool {
    if !glob.contains(&b'*') {
        return key == glob;
    }
    let mut rest = key;
    for (i, seg) in glob.split(|&b| b == b'*').enumerate() {
        if seg.is_empty() {
            continue;
        }
        if i == 0 {
            if !rest.starts_with(seg) {
                return false;
            }
            rest = &rest[seg.len()..];
        } else if let Some(pos) = rest.windows(seg.len()).position(|w| w == seg) {
            rest = &rest[pos + seg.len()..];
        } else {
            return false;
        }
    }
    true
}

/// Build a `section.subsection.key` full key (or `section.key` when there is no subsection).
fn full_key(section: String, subsection: Option<String>, key: String) -> String {
    match subsection {
        Some(sub) => format!("{section}.{sub}.{key}"),
        None => format!("{section}.{key}"),
    }
}

/// Split a `section.subsection.key` (or `section.key`) fully-qualified config key into its
/// parts, or error when it does not contain at least a section and a name.
fn parse_key(key: &str) -> napi::Result<(String, Option<String>, String)> {
    let parts: Vec<&str> = key.split('.').collect();
    if parts.len() < 2 {
        return Err(err_str(
            "parse_key",
            "config key must be of the form 'section.key' (or 'section.subsection.key')",
        ));
    }
    let section = parts[0].to_string();
    let name = parts[parts.len() - 1].to_string();
    let subsection = if parts.len() > 2 {
        Some(parts[1..parts.len() - 1].join("."))
    } else {
        None
    };
    Ok((section, subsection, name))
}

/// A single environment variable handed to a signing program.
#[napi(object)]
pub struct SigningEnvVar {
    pub name: String,
    pub value: String,
}

/// Fully resolved options for signing a commit or annotated tag.
#[napi(object)]
pub struct SigningOptions {
    /// The signature format (e.g. `OpenSsh`, `Ssh`, `Gpg`, `Pgp`).
    pub format: String,
    /// The external signing program.
    pub program: String,
    /// Additional arguments passed to the signing program before Git's fixed arguments.
    pub program_arguments: Vec<String>,
    /// The key, identity, or key path passed to the signing program.
    pub signing_key: String,
    /// Environment variables set only for the signing program.
    pub environment: Vec<SigningEnvVar>,
}

/// Repository context that gets injected into spawned programs.
#[napi(object)]
pub struct CommandContext {
    /// The `.git` directory that contains the repository, or `null`.
    pub git_dir: Option<String>,
    /// The worktree directory, or `null`.
    pub worktree_dir: Option<String>,
    /// Whether to set `GIT_NO_REPLACE_OBJECTS`, or `null`.
    pub no_replace_objects: Option<bool>,
    /// The reference namespace, or `null`.
    pub ref_namespace: Option<String>,
    /// Whether to set `GIT_LITERAL_PATHSPECS`, or `null`.
    pub literal_pathspecs: Option<bool>,
    /// Whether to set `GIT_GLOB_PATHSPECS`, or `null`.
    pub glob_pathspecs: Option<bool>,
    /// Whether to set `GIT_ICASE_PATHSPECS`, or `null`.
    pub icase_pathspecs: Option<bool>,
    /// Whether to inherit `stderr`, or `null`.
    pub stderr: Option<bool>,
}

/// A locked, single-file configuration transaction opened with
/// [`Repository::configFileMut`]. Dropping it releases the lock without writing;
/// call [`commit()`](ConfigTransaction::commit) to persist changes.
#[napi]
pub struct ConfigTransaction {
    inner: Option<gix::config::FileTransaction>,
}

/// A configuration file source, for [`Repository::config_path`].
#[napi]
pub enum ConfigSource {
    /// A special configuration file that ships with the git installation.
    GitInstallation,
    /// System-wide configuration, e.g. `$(prefix)/etc/gitconfig`.
    System,
    /// Platform-defined user configuration, e.g. `$XDG_CONFIG_HOME/git/config`.
    Git,
    /// The user's configuration, usually `~/.gitconfig`.
    User,
    /// The repository's configuration in `common_dir/config`.
    Local,
    /// Per-worktree configuration, `config.worktree` with `extensions.worktreeConfig`.
    Worktree,
    /// Values parsed from `GIT_CONFIG_COUNT` and friends.
    Env,
    /// Values set from the command-line.
    Cli,
    /// Values from a programmatic source.
    Api,
    /// Values from specific environment variables like `HTTP_PROXY`.
    EnvOverride,
}

fn to_config_source(s: ConfigSource) -> gix::config::Source {
    use gix::config::Source as S;
    match s {
        ConfigSource::GitInstallation => S::GitInstallation,
        ConfigSource::System => S::System,
        ConfigSource::Git => S::Git,
        ConfigSource::User => S::User,
        ConfigSource::Local => S::Local,
        ConfigSource::Worktree => S::Worktree,
        ConfigSource::Env => S::Env,
        ConfigSource::Cli => S::Cli,
        ConfigSource::Api => S::Api,
        ConfigSource::EnvOverride => S::EnvOverride,
    }
}

#[napi]
impl Repository {
    /// Read a single config value as a string.
    #[napi]
    pub fn config_string(&self, key: String) -> Option<String> {
        let r = thread_local(&self.inner);
        r.config_snapshot().string(key.as_str()).map(|v| lossy(&*v))
    }

    #[napi]
    pub fn config_snapshot(&self) -> napi::Result<ConfigSnapshot> {
        let r = thread_local(&self.inner);
        let snap = r.config_snapshot();
        // `gix::config::Snapshot` borrows the thread-local repository, so clone the
        // underlying owned config file to keep it valid beyond this call.
        Ok(ConfigSnapshot {
            inner: (*snap).clone(),
        })
    }

    /// Return all flattened configuration key/value entries. When `glob` is provided, only
    /// entries whose fully-qualified key matches the pattern (a simplified `*`-only fnmatch)
    /// are returned.
    #[napi]
    pub fn config_entries(&self, glob: Option<String>) -> napi::Result<Vec<ConfigEntry>> {
        let r = thread_local(&self.inner);
        let snap = r.config_snapshot();
        let mut out = Vec::new();
        for section in snap.sections() {
            let header = section.header();
            let section_name = header.name();
            let sub = header.subsection_name();
            let meta = section.meta();
            for key in section.value_names() {
                let key_str = if let Some(sub_name) = sub {
                    format!("{section_name}.{sub_name}.{key}")
                } else {
                    format!("{section_name}.{key}")
                };
                if let Some(glob) = &glob {
                    if !key_glob_match(key_str.as_bytes(), glob.as_bytes()) {
                        continue;
                    }
                }
                if let Some(val) = section.value(&key) {
                    out.push(ConfigEntry {
                        key: key_str,
                        value: lossy(val.as_ref()),
                        level: Some(source_name(meta.source)),
                        include_depth: Some(meta.level as u32),
                    });
                }
            }
        }
        Ok(out)
    }

    /// Set a configuration key to a value in local config and snapshot.
    #[napi]
    pub fn config_set(&self, key: String, value: String) -> napi::Result<()> {
        let mut r = thread_local(&self.inner);
        let local_config_path = r.git_dir().join("config");
        let mut local_file = if local_config_path.exists() {
            gix::config::File::from_path_no_includes(
                local_config_path.clone(),
                gix::config::Source::Local,
            )
            .ok()
        } else {
            None
        };

        let mut snapshot = r.config_snapshot_mut();
        snapshot
            .set_raw_value(&key, value.as_str())
            .map_err(|e| err("config_set", e))?;
        let _ = snapshot.commit();

        if let Some(ref mut file) = local_file {
            let _ = file.set_raw_value(&key, value.as_str());
            let _ = std::fs::write(&local_config_path, file.to_bstring().as_slice());
        }
        Ok(())
    }

    /// Set a git config value in the local config file specifically (`.git/config`).
    #[napi]
    pub fn config_set_local(&self, key: String, value: String) -> napi::Result<()> {
        self.config_set(key, value)
    }

    /// Read a boolean configuration value. Returns `null` when absent/unset or if the key
    /// is not a valid boolean.
    #[napi]
    pub fn config_get_bool(&self, key: String) -> Option<bool> {
        let r = thread_local(&self.inner);
        r.config_snapshot()
            .boolean(key.as_str())
            .and_then(|res| res.ok())
    }

    /// Remove a configuration key from the repository's local config file. This mirrors
    /// `configSet()` but deletes the entry instead of writing it. After this call the
    /// in-memory repository configuration is stale — call `reload()` to pick up the change.
    #[napi]
    pub fn config_remove(&self, key: String) -> napi::Result<()> {
        let r = thread_local(&self.inner);
        let local_config_path = r.git_dir().join("config").to_string_lossy().into_owned();
        let (section, subsection, name) = parse_key(&key)?;
        let mut tx = self.config_file_mut(local_config_path)?;
        tx.remove_raw_value(section, subsection, name)?;
        tx.commit()?;
        Ok(())
    }

    /// Return the path at which a configuration file is expected, selected by its
    /// source. The file and its parents do not have to exist. Relative paths are
    /// resolved against the current directory captured when this repository was opened.
    #[napi]
    pub fn config_path(&self, source: ConfigSource) -> napi::Result<String> {
        let r = thread_local(&self.inner);
        let path = r
            .config_path(to_config_source(source))
            .map_err(|e| err("config_path", e))?;
        Ok(path.to_string_lossy().into_owned())
    }

    /// Return the editor program selected by Git's precedence rules, or `null` when none
    /// is configured and no terminal editor is available.
    #[napi]
    pub fn editor(&self) -> napi::Result<Option<String>> {
        let r = thread_local(&self.inner);
        Ok(r.editor()
            .map_err(|e| err("editor", e))?
            .map(|e| e.to_string_lossy().into_owned()))
    }

    /// Resolve all Git configuration needed to sign a commit with
    /// `gix_object::Commit::sign()`.
    #[napi]
    pub fn commit_signing_options(&self) -> napi::Result<SigningOptions> {
        let r = thread_local(&self.inner);
        let opts = r
            .commit_signing_options()
            .map_err(|e| err("commit_signing_options", e))?;
        Ok(signing_options_to_napi(opts))
    }

    /// Like [`commitSigningOptions()`](Repository::commitSigningOptions), but returns
    /// `null` when signing is disabled by `commit.gpgSign`.
    #[napi]
    pub fn commit_signing_options_if_enabled(&self) -> napi::Result<Option<SigningOptions>> {
        let r = thread_local(&self.inner);
        Ok(r.commit_signing_options_if_enabled()
            .map_err(|e| err("commit_signing_options_if_enabled", e))?
            .map(signing_options_to_napi))
    }

    /// Resolve the repository context injected into spawned programs.
    #[napi]
    pub fn command_context(&self) -> napi::Result<CommandContext> {
        let r = thread_local(&self.inner);
        let ctx = r.command_context().map_err(|e| err("command_context", e))?;
        Ok(CommandContext {
            git_dir: ctx.git_dir.map(|p| p.to_string_lossy().into_owned()),
            worktree_dir: ctx.worktree_dir.map(|p| p.to_string_lossy().into_owned()),
            no_replace_objects: ctx.no_replace_objects,
            ref_namespace: ctx
                .ref_namespace
                .map(|b| String::from_utf8_lossy(&b).into_owned()),
            literal_pathspecs: ctx.literal_pathspecs,
            glob_pathspecs: ctx.glob_pathspecs,
            icase_pathspecs: ctx.icase_pathspecs,
            stderr: ctx.stderr,
        })
    }

    /// Lock and open a single physical configuration file for editing. Relative paths are
    /// resolved against the current directory captured when this repository was opened.
    ///
    /// Dropping the returned transaction releases the lock without writing; call
    /// [`commit()`](ConfigTransaction::commit) to persist. Call [`reload()`](Repository::reload)
    /// afterwards to rebuild this repository instance from the changed configuration.
    #[napi]
    pub fn config_file_mut(&self, path: String) -> napi::Result<ConfigTransaction> {
        let r = thread_local(&self.inner);
        let tx = r
            .config_file_mut(path)
            .map_err(|e| err("config_file_mut", e))?;
        Ok(ConfigTransaction { inner: Some(tx) })
    }

    /// The compression level (0-9) used when writing loose objects (`core.looseCompression`).
    #[napi]
    pub fn loose_compression(&self) -> f64 {
        let r = thread_local(&self.inner);
        r.loose_compression().level() as f64
    }

    /// The effective compression level (0-9) used when writing pack entries
    /// (`pack.compression`).
    #[napi]
    pub fn pack_compression(&self) -> napi::Result<f64> {
        let r = thread_local(&self.inner);
        Ok(r.pack_compression()
            .map_err(|e| err("pack_compression", e))?
            .level() as f64)
    }

    /// The big-file threshold in bytes above which git will not diff or delta-diff
    /// (`core.bigFileThreshold`).
    #[napi]
    pub fn big_file_threshold(&self) -> napi::Result<f64> {
        let r = thread_local(&self.inner);
        Ok(r.big_file_threshold()
            .map_err(|e| err("big_file_threshold", e))? as f64)
    }

    /// The algorithm used for diffs and merges (`diff.algorithm`), Debug-formatted
    /// (`Histogram` or `Myers`).
    #[napi]
    pub fn diff_algorithm(&self) -> napi::Result<String> {
        let r = thread_local(&self.inner);
        Ok(format!(
            "{:?}",
            r.diff_algorithm().map_err(|e| err("diff_algorithm", e))?
        ))
    }
}

/// Convert gix's resolved signing options into the napi representation.
fn signing_options_to_napi(opts: gix::objs::signature::sign::Options) -> SigningOptions {
    SigningOptions {
        format: format!("{:?}", opts.format),
        program: opts.program.to_string_lossy().into_owned(),
        program_arguments: opts
            .program_arguments
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect(),
        signing_key: opts.signing_key.to_string_lossy().into_owned(),
        environment: opts
            .environment
            .into_iter()
            .map(|(name, value)| SigningEnvVar {
                name: name.to_string_lossy().into_owned(),
                value: value.to_string_lossy().into_owned(),
            })
            .collect(),
    }
}

#[napi]
impl ConfigTransaction {
    /// Set a configuration key to `value`, creating the section if needed. `subsection` may
    /// be `null` for top-level sections.
    #[napi]
    pub fn set_raw_value(
        &mut self,
        section: String,
        subsection: Option<String>,
        key: String,
        value: String,
    ) -> napi::Result<()> {
        let tx = self
            .inner
            .as_mut()
            .ok_or_else(|| napi::Error::from_reason("transaction already finalized"))?;
        let full_key = match subsection {
            Some(sub) => format!("{}.{}.{}", section, sub, key),
            None => format!("{}.{}", section, key),
        };
        tx.set_raw_value(full_key.as_str(), value.as_str())
            .map(|_| ())
            .map_err(|e| err("set_raw_value", e))
    }

    /// Remove an entire section (and its subsection) from the file.
    #[napi]
    pub fn remove_section(
        &mut self,
        section: String,
        subsection: Option<String>,
    ) -> napi::Result<()> {
        let tx = self
            .inner
            .as_mut()
            .ok_or_else(|| napi::Error::from_reason("transaction already finalized"))?;
        tx.remove_section(
            section.as_str(),
            subsection.as_deref().map(gix::bstr::BStr::new),
        );
        Ok(())
    }

    /// Remove a single entry (all its values) at `section`/`subsection`/`key`, leaving the
    /// rest of the section intact. Errors when the section does not exist.
    #[napi]
    pub fn remove_raw_value(
        &mut self,
        section: String,
        subsection: Option<String>,
        key: String,
    ) -> napi::Result<()> {
        let tx = self
            .inner
            .as_mut()
            .ok_or_else(|| napi::Error::from_reason("transaction already finalized"))?;
        let mut sec = tx
            .section_mut(
                section.as_str(),
                subsection.as_deref().map(gix::bstr::BStr::new),
            )
            .map_err(|e| err("remove_raw_value", e))?;
        sec.remove(key.as_str());
        Ok(())
    }

    /// Set a boolean entry, writing `true`/`false` (mirrors libgit2 `set_bool`).
    #[napi]
    pub fn set_bool(
        &mut self,
        section: String,
        subsection: Option<String>,
        key: String,
        value: bool,
    ) -> napi::Result<()> {
        let full = full_key(section, subsection, key);
        let tx = self
            .inner
            .as_mut()
            .ok_or_else(|| napi::Error::from_reason("transaction already finalized"))?;
        tx.set_raw_value(full.as_str(), if value { "true" } else { "false" })
            .map(|_| ())
            .map_err(|e| err("set_bool", e))
    }

    /// Set a 32-bit integer entry (mirrors libgit2 `set_i32`).
    #[napi]
    pub fn set_int32(
        &mut self,
        section: String,
        subsection: Option<String>,
        key: String,
        value: i32,
    ) -> napi::Result<()> {
        let full = full_key(section, subsection, key);
        let tx = self
            .inner
            .as_mut()
            .ok_or_else(|| napi::Error::from_reason("transaction already finalized"))?;
        tx.set_raw_value(full.as_str(), value.to_string().as_str())
            .map(|_| ())
            .map_err(|e| err("set_int32", e))
    }

    /// Set a 64-bit integer entry (mirrors libgit2 `set_i64`).
    #[napi]
    pub fn set_int64(
        &mut self,
        section: String,
        subsection: Option<String>,
        key: String,
        value: i64,
    ) -> napi::Result<()> {
        let full = full_key(section, subsection, key);
        let tx = self
            .inner
            .as_mut()
            .ok_or_else(|| napi::Error::from_reason("transaction already finalized"))?;
        tx.set_raw_value(full.as_str(), value.to_string().as_str())
            .map(|_| ())
            .map_err(|e| err("set_int64", e))
    }

    /// Set a multivar entry: every existing value matching `regexp` is replaced with `value`;
    /// if none match, `value` is appended. The section is created if it does not yet exist.
    #[napi]
    pub fn set_multivar(
        &mut self,
        section: String,
        subsection: Option<String>,
        key: String,
        regexp: String,
        value: String,
    ) -> napi::Result<()> {
        let tx = self
            .inner
            .as_mut()
            .ok_or_else(|| napi::Error::from_reason("transaction already finalized"))?;
        let re = regex::Regex::new(&regexp).map_err(|e| err_str("set_multivar", e.to_string()))?;
        let mut sec = tx
            .section_mut_or_create_new(
                section.as_str(),
                subsection.as_deref().map(gix::bstr::BStr::new),
            )
            .map_err(|e| err("set_multivar", e))?;
        let values: Vec<gix::bstr::BString> = sec.values(key.as_str()).to_vec();
        let matches = values
            .iter()
            .filter(|v| re.is_match(&lossy(v.as_ref())))
            .count();
        loop {
            let cur: Vec<gix::bstr::BString> = sec.values(key.as_str()).to_vec();
            if !cur.iter().any(|v| re.is_match(&lossy(v.as_ref()))) {
                break;
            }
            sec.remove(key.as_str());
        }
        let n = if matches > 0 { matches } else { 1 };
        for _ in 0..n {
            sec.push(key.as_str(), value.as_str())
                .map_err(|e| err("set_multivar", e))?;
        }
        Ok(())
    }

    /// Remove every value of `key` matching `regexp`. Returns the number of values removed.
    #[napi]
    pub fn remove_multivar(
        &mut self,
        section: String,
        subsection: Option<String>,
        key: String,
        regexp: String,
    ) -> napi::Result<u32> {
        let tx = self
            .inner
            .as_mut()
            .ok_or_else(|| napi::Error::from_reason("transaction already finalized"))?;
        let re =
            regex::Regex::new(&regexp).map_err(|e| err_str("remove_multivar", e.to_string()))?;
        let mut sec = tx
            .section_mut_or_create_new(
                section.as_str(),
                subsection.as_deref().map(gix::bstr::BStr::new),
            )
            .map_err(|e| err("remove_multivar", e))?;
        let mut removed = 0u32;
        loop {
            let cur: Vec<gix::bstr::BString> = sec.values(key.as_str()).to_vec();
            if !cur.iter().any(|v| re.is_match(&lossy(v.as_ref()))) {
                break;
            }
            sec.remove(key.as_str());
            removed += 1;
        }
        Ok(removed)
    }

    /// Write the modified file atomically and release the lock.
    #[napi]
    pub fn commit(&mut self) -> napi::Result<()> {
        let Some(tx) = self.inner.take() else {
            return Err(napi::Error::from_reason("transaction already finalized"));
        };
        tx.commit().map_err(|e| err("commit", e))
    }

    /// Release the lock without writing any changes.
    #[napi]
    pub fn discard(&mut self) -> napi::Result<()> {
        // Dropping the inner transaction releases the file lock without writing.
        self.inner.take();
        Ok(())
    }
}

#[napi]
pub struct ConfigSnapshot {
    pub(crate) inner: gix::config::File,
}

#[napi]
impl ConfigSnapshot {
    /// Read a single configuration value as a string, or `null` if absent.
    #[napi]
    pub fn string(&self, key: String) -> Option<String> {
        self.inner.string(key.as_str()).map(|v| lossy(&v))
    }

    /// Read all values for a multivar key, or `null` if the key is absent.
    #[napi]
    pub fn strings(&self, key: String) -> Option<Vec<String>> {
        self.inner
            .strings(key.as_str())
            .map(|vs| vs.iter().map(|v| lossy(v)).collect())
    }

    /// Read a boolean value, falling back to `defaultValue` when absent or unset.
    #[napi]
    pub fn boolean(&self, key: String, default_value: Option<bool>) -> napi::Result<bool> {
        Ok(self
            .inner
            .boolean(key.as_str())
            .map_err(|e| err("config_snapshot.boolean", e))?
            .unwrap_or(default_value.unwrap_or(false)))
    }

    /// Read an integer value, falling back to `defaultValue` when absent.
    #[napi]
    pub fn integer(&self, key: String, default_value: Option<i64>) -> napi::Result<i64> {
        Ok(self
            .inner
            .integer(key.as_str())
            .map_err(|e| err("config_snapshot.integer", e))?
            .unwrap_or(default_value.unwrap_or(0)))
    }

    /// All flattened configuration key/value entries. When `glob` is provided, only entries
    /// whose fully-qualified key matches the pattern (a simplified `*`-only fnmatch) are
    /// returned. Each entry carries the `level` and `includeDepth` it was read from.
    #[napi]
    pub fn entries(&self, glob: Option<String>) -> Vec<ConfigEntry> {
        let mut out = Vec::new();
        for section in self.inner.sections() {
            let header = section.header();
            let section_name = header.name();
            let sub = header.subsection_name();
            let meta = section.meta();
            for key in section.value_names() {
                let key_str = if let Some(sub_name) = sub {
                    format!("{section_name}.{sub_name}.{key}")
                } else {
                    format!("{section_name}.{key}")
                };
                if let Some(glob) = &glob {
                    if !key_glob_match(key_str.as_bytes(), glob.as_bytes()) {
                        continue;
                    }
                }
                if let Some(val) = section.value(&key) {
                    out.push(ConfigEntry {
                        key: key_str,
                        value: lossy(val.as_ref()),
                        level: Some(source_name(meta.source)),
                        include_depth: Some(meta.level as u32),
                    });
                }
            }
        }
        out
    }

    /// Read a boolean value, erroring when the key is absent or its value is not a valid
    /// boolean (mirrors libgit2 `get_bool`). Contrast with `boolean()`, which falls back to a
    /// default.
    #[napi(js_name = "bool")]
    pub fn bool_value(&self, key: String) -> napi::Result<bool> {
        self.inner
            .boolean(key.as_str())
            .map_err(|e| err("config_snapshot.bool", e))?
            .ok_or_else(|| {
                err_str(
                    "config_snapshot.bool",
                    format!("configuration key '{key}' not found or has no value"),
                )
            })
    }

    /// Read an integer value, erroring when the key is absent or its value is not a valid
    /// integer (mirrors libgit2 `get_i64`). Contrast with `integer()`, which falls back to a
    /// default.
    #[napi]
    pub fn int(&self, key: String) -> napi::Result<i64> {
        self.inner
            .integer(key.as_str())
            .map_err(|e| err("config_snapshot.int", e))?
            .ok_or_else(|| {
                err_str(
                    "config_snapshot.int",
                    format!("configuration key '{key}' not found or has no value"),
                )
            })
    }

    /// Read a path value, expanding a leading `~` to the current user's home directory.
    /// Returns `null` when the key is absent.
    #[napi]
    pub fn path(&self, key: String) -> Option<String> {
        let s = self.inner.string(key.as_str())?;
        let mut p = lossy(s.as_ref());
        if p.starts_with('~') {
            if let Ok(home) = std::env::var("HOME") {
                p = p.replacen('~', &home, 1);
            }
        }
        Some(p)
    }

    /// Return the highest-precedence entry for `key` (the last definition in load order),
    /// or `null` when the key is absent. The returned entry includes its `level` and
    /// `includeDepth`.
    #[napi]
    pub fn entry(&self, key: String) -> Option<ConfigEntry> {
        let (section, subsection, name) = parse_key(&key).ok()?;
        let sub = subsection.as_deref().map(gix::bstr::BStr::new);
        let mut found: Option<ConfigEntry> = None;
        for s in self.inner.sections() {
            let h = s.header();
            if h.name() != section.as_str() {
                continue;
            }
            match (h.subsection_name(), sub) {
                (Some(a), Some(b)) if a == b => {}
                (None, None) => {}
                _ => continue,
            }
            if let Some(val) = s.value(name.as_str()) {
                let meta = s.meta();
                found = Some(ConfigEntry {
                    key: key.clone(),
                    value: lossy(val.as_ref()),
                    level: Some(source_name(meta.source)),
                    include_depth: Some(meta.level as u32),
                });
            }
        }
        found
    }
}
