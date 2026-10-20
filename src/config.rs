use napi::Result;
use napi_derive::napi;

use crate::error::{err, thread_local};
use crate::repository::Repository;

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// A single configuration key/value entry.
#[derive(Clone)]
#[napi(object)]
pub struct ConfigEntry {
    pub key: String,
    pub value: String,
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
    pub fn config_snapshot(&self) -> Result<ConfigSnapshot> {
        let r = thread_local(&self.inner);
        let snap = r.config_snapshot();
        // `gix::config::Snapshot` borrows the thread-local repository, so clone the
        // underlying owned config file to keep it valid beyond this call.
        Ok(ConfigSnapshot {
            inner: (*snap).clone(),
        })
    }

    /// Return all flattened configuration key/value entries.
    #[napi]
    pub fn config_entries(&self) -> Result<Vec<ConfigEntry>> {
        let r = thread_local(&self.inner);
        let snap = r.config_snapshot();
        let mut out = Vec::new();
        for section in snap.sections() {
            let header = section.header();
            let section_name = header.name();
            let sub = header.subsection_name();
            for key in section.value_names() {
                let key_str = if let Some(sub_name) = sub {
                    format!("{section_name}.{sub_name}.{key}")
                } else {
                    format!("{section_name}.{key}")
                };
                if let Some(val) = section.value(&key) {
                    out.push(ConfigEntry {
                        key: key_str,
                        value: lossy(val.as_ref()),
                    });
                }
            }
        }
        Ok(out)
    }

    /// Set a configuration key to a value in local config and snapshot.
    #[napi]
    pub fn config_set(&self, key: String, value: String) -> Result<()> {
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

    /// Return the path at which a configuration file is expected, selected by its
    /// source. The file and its parents do not have to exist. Relative paths are
    /// resolved against the current directory captured when this repository was opened.
    #[napi]
    pub fn config_path(&self, source: ConfigSource) -> Result<String> {
        let r = thread_local(&self.inner);
        let path = r
            .config_path(to_config_source(source))
            .map_err(|e| err("config_path", e))?;
        Ok(path.to_string_lossy().into_owned())
    }

    /// Return the editor program selected by Git's precedence rules, or `null` when none
    /// is configured and no terminal editor is available.
    #[napi]
    pub fn editor(&self) -> Result<Option<String>> {
        let r = thread_local(&self.inner);
        Ok(r.editor()
            .map_err(|e| err("editor", e))?
            .map(|e| e.to_string_lossy().into_owned()))
    }

    /// Resolve all Git configuration needed to sign a commit with
    /// `gix_object::Commit::sign()`.
    #[napi]
    pub fn commit_signing_options(&self) -> Result<SigningOptions> {
        let r = thread_local(&self.inner);
        let opts = r
            .commit_signing_options()
            .map_err(|e| err("commit_signing_options", e))?;
        Ok(signing_options_to_napi(opts))
    }

    /// Like [`commitSigningOptions()`](Repository::commitSigningOptions), but returns
    /// `null` when signing is disabled by `commit.gpgSign`.
    #[napi]
    pub fn commit_signing_options_if_enabled(&self) -> Result<Option<SigningOptions>> {
        let r = thread_local(&self.inner);
        Ok(r.commit_signing_options_if_enabled()
            .map_err(|e| err("commit_signing_options_if_enabled", e))?
            .map(signing_options_to_napi))
    }

    /// Resolve the repository context injected into spawned programs.
    #[napi]
    pub fn command_context(&self) -> Result<CommandContext> {
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
    pub fn config_file_mut(&self, path: String) -> Result<ConfigTransaction> {
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
    pub fn pack_compression(&self) -> Result<f64> {
        let r = thread_local(&self.inner);
        Ok(r.pack_compression()
            .map_err(|e| err("pack_compression", e))?
            .level() as f64)
    }

    /// The big-file threshold in bytes above which git will not diff or delta-diff
    /// (`core.bigFileThreshold`).
    #[napi]
    pub fn big_file_threshold(&self) -> Result<f64> {
        let r = thread_local(&self.inner);
        Ok(r.big_file_threshold()
            .map_err(|e| err("big_file_threshold", e))? as f64)
    }

    /// The algorithm used for diffs and merges (`diff.algorithm`), Debug-formatted
    /// (`Histogram` or `Myers`).
    #[napi]
    pub fn diff_algorithm(&self) -> Result<String> {
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
    ) -> Result<()> {
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
    pub fn remove_section(&mut self, section: String, subsection: Option<String>) -> Result<()> {
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

    /// Write the modified file atomically and release the lock.
    #[napi]
    pub fn commit(&mut self) -> Result<()> {
        let Some(tx) = self.inner.take() else {
            return Err(napi::Error::from_reason("transaction already finalized"));
        };
        tx.commit().map_err(|e| err("commit", e))
    }

    /// Release the lock without writing any changes.
    #[napi]
    pub fn discard(&mut self) -> Result<()> {
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
    pub fn boolean(&self, key: String, default_value: Option<bool>) -> Result<bool> {
        Ok(self
            .inner
            .boolean(key.as_str())
            .map_err(|e| err("config_snapshot.boolean", e))?
            .unwrap_or(default_value.unwrap_or(false)))
    }

    /// Read an integer value, falling back to `defaultValue` when absent.
    #[napi]
    pub fn integer(&self, key: String, default_value: Option<i64>) -> Result<i64> {
        Ok(self
            .inner
            .integer(key.as_str())
            .map_err(|e| err("config_snapshot.integer", e))?
            .unwrap_or(default_value.unwrap_or(0)))
    }

    /// All flattened configuration key/value entries.
    #[napi]
    pub fn entries(&self) -> Vec<ConfigEntry> {
        let mut out = Vec::new();
        for section in self.inner.sections() {
            let header = section.header();
            let section_name = header.name();
            let sub = header.subsection_name();
            for key in section.value_names() {
                let key_str = if let Some(sub_name) = sub {
                    format!("{section_name}.{sub_name}.{key}")
                } else {
                    format!("{section_name}.{key}")
                };
                if let Some(val) = section.value(&key) {
                    out.push(ConfigEntry {
                        key: key_str,
                        value: lossy(val.as_ref()),
                    });
                }
            }
        }
        out
    }
}
