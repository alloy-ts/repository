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
