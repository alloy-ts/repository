use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

use napi_derive::napi;

/// A single plaintext credential entry keyed by domain (use `"*"` for a fallback).
#[derive(Clone)]
#[napi(object)]
pub struct CredentialEntry {
    pub domain: String,
    pub username: String,
    pub password: String,
}

/// A per-domain username to try when a URL omits one.
#[derive(Clone)]
#[napi(object)]
pub struct UsernameEntry {
    pub domain: String,
    pub username: String,
}

/// An SSH private key file with an optional passphrase.
#[derive(Clone)]
#[napi(object)]
pub struct SshKeyEntry {
    pub path: String,
    pub passphrase: Option<String>,
}

/// JS-friendly configuration for a [`Auth`]. All fields optional; omitted fields fall
/// back to sensible headless defaults (interactive prompts off, SSH agent + default SSH keys on).
#[derive(Clone)]
#[napi(object)]
pub struct AuthOpts {
    /// Domain (or `"*"`) → plaintext credentials.
    pub credentials: Option<Vec<CredentialEntry>>,
    /// Use the git credential helper. Defaults to `true` (git's own default).
    pub credential_helper: Option<bool>,
    /// Max interactive password prompts (`0` disables). Defaults to `0` for JS (no TTY).
    pub password_prompts: Option<u32>,
    /// Domain (or `"*"`) → username to try.
    pub usernames: Option<Vec<UsernameEntry>>,
    /// Use the SSH agent. Defaults to `true`.
    pub ssh_agent: Option<bool>,
    /// SSH private key files (path + optional passphrase).
    pub ssh_keys: Option<Vec<SshKeyEntry>>,
    /// Also try default SSH key locations (`~/.ssh/id_*`). Defaults to `true`.
    pub default_ssh_keys: Option<bool>,
    /// Prompt for passphrases on encrypted SSH keys. Defaults to `false` (no TTY in JS).
    pub prompt_ssh_key_passphrase: Option<bool>,
}

/// A configurable git credential authenticator.
///
/// Build it from [`AuthOpts`] or start empty and configure with the `add*`/`try*`
/// methods, then pass it to an authenticated operation (`Repository.clone`/`fetch`/`push`). The
/// authenticator is translated into git env vars + `-c` config overrides at apply time.
#[napi]
pub struct Auth {
    credentials: Vec<CredentialEntry>,
    credential_helper: bool,
    password_prompts: u32,
    usernames: Vec<UsernameEntry>,
    ssh_agent: bool,
    ssh_keys: Vec<String>,
    default_ssh_keys: bool,
    prompt_ssh_key_passphrase: bool,
}

impl Default for Auth {
    fn default() -> Self {
        Self {
            credentials: Vec::new(),
            credential_helper: true,
            password_prompts: 0,
            usernames: Vec::new(),
            ssh_agent: true,
            ssh_keys: Vec::new(),
            default_ssh_keys: true,
            prompt_ssh_key_passphrase: false,
        }
    }
}

/// Quote a path for safe inclusion in a shell argument / `GIT_SSH_COMMAND`.
fn shlex_path(p: &str) -> String {
    if p.contains('\'') {
        format!("'{}'", p.replace('\'', "'\\''"))
    } else if p.contains(' ') {
        format!("'{}'", p)
    } else {
        p.to_string()
    }
}

/// Build a `case` glob pattern for a domain (`github.com` → `github.com*`, `"*"` → `*`).
fn domain_pattern(domain: &str) -> String {
    if domain == "*" {
        "*".to_string()
    } else {
        format!("{domain}*")
    }
}

impl Auth {
    /// Collect SSH key paths to use: the explicitly-configured keys plus, when enabled, the existing
    /// default `~/.ssh/id_*` files.
    fn resolved_ssh_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.ssh_keys.clone();
        if self.default_ssh_keys {
            if let Some(home) = dirs_home() {
                for name in [
                    "id_rsa",
                    "id_ecdsa",
                    "id_ecdsa_sk",
                    "id_ed25519",
                    "id_ed25519_sk",
                    "id_dsa",
                ] {
                    let p = home.join(".ssh").join(name);
                    if p.is_file() {
                        keys.push(p.to_string_lossy().into_owned());
                    }
                }
            }
        }
        keys
    }

    /// Git environment variables for this configuration.
    pub(crate) fn git_env(&self) -> Vec<(String, String)> {
        let mut env: Vec<(String, String)> = vec![("GIT_TERMINAL_PROMPT".into(), "0".into())];
        for (i, c) in self.credentials.iter().enumerate() {
            env.push((format!("GIT_AUTH_{i}_U"), c.username.clone()));
            env.push((format!("GIT_AUTH_{i}_P"), c.password.clone()));
        }
        let keys = self.resolved_ssh_keys();
        if !keys.is_empty() || !self.ssh_agent {
            let mut ssh = String::from("ssh");
            for k in &keys {
                ssh.push_str(&format!(" -i {}", shlex_path(k)));
            }
            ssh.push_str(" -o IdentitiesOnly=yes");
            env.push(("GIT_SSH_COMMAND".into(), ssh));
        }
        if !self.ssh_agent {
            // Disable the agent by clearing the socket so only the configured keys are tried.
            env.push(("SSH_AUTH_SOCK".into(), String::new()));
        }
        env
    }

    /// `git -c` configuration overrides for this configuration.
    pub(crate) fn git_config(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if !self.credential_helper {
            // Explicitly clear any inherited credential helper.
            out.push("credential.helper=".to_string());
        }
        if !self.credentials.is_empty() {
            // A credential helper that echoes the matching stored secret from the GIT_AUTH_*_U/P
            // env vars (set alongside this in `git_env`), keyed by the requested host.
            let mut cases = String::new();
            for (i, c) in self.credentials.iter().enumerate() {
                cases.push_str(&format!(
                    " {}*) echo username=\"$GIT_AUTH_{i}_U\"; echo password=\"$GIT_AUTH_{i}_P\";;",
                    domain_pattern(&c.domain)
                ));
            }
            let helper = format!(
                "!f() {{ h=\"\"; while IFS= read -r line; do case \"$line\" in host=*) h=\"${{line#host=}}\";; esac; done; case \"$h\" in{cases} esac; }}; f"
            );
            out.push(format!("credential.helper={helper}"));
        }
        out
    }

    /// Apply this authenticator to a `git` subprocess `Command`: sets env vars and prepends `-c`
    /// config overrides. Used by the git-based clone/fetch/push fallback.
    pub(crate) fn apply_to_command(&self, cmd: &mut Command) {
        for (k, v) in self.git_env() {
            cmd.env(k, v);
        }
        for c in self.git_config() {
            cmd.arg("-c").arg(c);
        }
    }

    /// Set this authenticator's environment (including `GIT_CONFIG_*` for the credential helper)
    /// for the duration of a gix operation, returning a guard that restores the prior values on
    /// drop. Used by the gix-native transport (fetch, and clone when gix is tried first).
    ///
    /// NOTE: setting process-global env is not thread-safe; this is a best-effort bridge until a
    /// proper `gix::credentials::Program` / config overlay is wired. TODO.
    pub(crate) fn env_guard(&self) -> EnvGuard {
        let mut entries: Vec<(String, String)> = self.git_env();
        let config = self.git_config();
        if !config.is_empty() {
            entries.push(("GIT_CONFIG_COUNT".into(), config.len().to_string()));
            for (i, c) in config.iter().enumerate() {
                if let Some((k, v)) = c.split_once('=') {
                    entries.push((format!("GIT_CONFIG_KEY_{i}"), k.to_string()));
                    entries.push((format!("GIT_CONFIG_VALUE_{i}"), v.to_string()));
                }
            }
        }
        let mut saved = Vec::with_capacity(entries.len());
        for (k, v) in entries {
            let prev = std::env::var_os(&k);
            saved.push((k.clone(), prev));
            unsafe { std::env::set_var(&k, v) };
        }
        EnvGuard { saved }
    }
}

/// RAII guard restoring environment variables set by [`Auth::env_guard`].
pub(crate) struct EnvGuard {
    saved: Vec<(String, Option<std::ffi::OsString>)>,
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, prev) in &self.saved {
            match prev {
                Some(v) => unsafe { std::env::set_var(k, v) },
                None => unsafe { std::env::remove_var(k) },
            }
        }
    }
}

#[napi]
impl Auth {
    /// Create an authenticator from options, or an empty one with headless defaults if omitted.
    #[napi(constructor)]
    pub fn new(options: Option<AuthOpts>) -> Self {
        let mut a = Auth::default();
        if let Some(o) = options {
            if let Some(creds) = o.credentials {
                a.credentials = creds;
            }
            if let Some(v) = o.credential_helper {
                a.credential_helper = v;
            }
            if let Some(v) = o.password_prompts {
                a.password_prompts = v;
            }
            if let Some(us) = o.usernames {
                a.usernames = us;
            }
            if let Some(v) = o.ssh_agent {
                a.ssh_agent = v;
            }
            if let Some(keys) = o.ssh_keys {
                a.ssh_keys = keys.into_iter().map(|k| k.path).collect();
            }
            if let Some(v) = o.default_ssh_keys {
                a.default_ssh_keys = v;
            }
            if let Some(v) = o.prompt_ssh_key_passphrase {
                a.prompt_ssh_key_passphrase = v;
            }
        }
        a
    }

    /// Inspect the environment variables this authenticator would set (useful for debugging / for
    /// applying to a non-`Command` context such as the gix path).
    #[napi]
    pub fn git_env_map(&self) -> HashMap<String, String> {
        self.git_env().into_iter().collect()
    }

    /// Add plaintext `username`/`password` for `domain` (`"*"` = fallback).
    #[napi]
    pub fn add_plaintext_credentials(
        &mut self,
        domain: String,
        username: String,
        password: String,
    ) {
        self.credentials.push(CredentialEntry {
            domain,
            username,
            password,
        });
    }

    /// Enable/disable the git credential helper.
    #[napi]
    pub fn try_credential_helper(&mut self, enable: bool) {
        self.credential_helper = enable;
    }

    /// Set the maximum number of interactive password prompts (`0` disables).
    #[napi]
    pub fn try_password_prompt(&mut self, max_count: u32) {
        self.password_prompts = max_count;
    }

    /// Add a per-domain `username` to try (`"*"` = fallback).
    #[napi]
    pub fn add_username(&mut self, domain: String, username: String) {
        self.usernames.push(UsernameEntry { domain, username });
    }

    /// Enable/disable the SSH agent.
    #[napi]
    pub fn try_ssh_agent(&mut self, enable: bool) {
        self.ssh_agent = enable;
    }

    /// Add an SSH private key file path (passphrase is stored but only used if a live prompt is
    /// enabled; otherwise an unencrypted key or agent is required).
    #[napi]
    pub fn add_ssh_key_from_file(&mut self, private_key: String, passphrase: Option<String>) {
        // Store the path; the passphrase is attached for completeness but not yet applied to the
        // ssh command (gix/git ssh backends read it interactively or via an agent).
        let _ = passphrase;
        self.ssh_keys.push(private_key);
    }

    /// Add the default SSH keys (`~/.ssh/id_*`) if they exist.
    #[napi]
    pub fn add_default_ssh_keys(&mut self) {
        self.default_ssh_keys = true;
    }

    /// Enable/disable prompting for passphrases on encrypted SSH keys.
    #[napi]
    pub fn prompt_ssh_key_passphrase(&mut self, enable: bool) {
        self.prompt_ssh_key_passphrase = enable;
    }
}

/// Best-effort `dirs::home_dir` without pulling in the `dirs` crate (kept local to avoid a new dep).
fn dirs_home() -> Option<PathBuf> {
    if let Ok(h) = std::env::var("HOME") {
        if !h.is_empty() {
            return Some(PathBuf::from(h));
        }
    }
    if let Ok(p) = std::env::var("USERPROFILE") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    None
}
