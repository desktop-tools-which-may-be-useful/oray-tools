use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const DEFAULT_API_BASE: &str = "https://api-std.sunlogin.oray.com";
pub const DEFAULT_SLAPI_BASE: &str = "https://slapi.oray.net";
/// Shield service that sends SMS login codes (see `oray_core::auth`).
pub const DEFAULT_SHIELD_BASE: &str = oray_core::auth::SHIELD_BASE;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Account {
    pub account: String,
    /// md5 hex (lowercase) of the plaintext password
    pub password_md5: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Token {
    pub access_token: String,
    pub refresh_token: String,
    /// absolute unix timestamp when refresh_token expires
    pub refresh_expires: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Client {
    pub clientid: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Server {
    pub api_base: String,
    pub slapi_base: String,
    /// Shield service that sends SMS login codes. Omitted by older configs,
    /// hence the default.
    #[serde(default)]
    pub shield_base: String,
}

/// Local configuration. Only authentication material is stored: account,
/// trusted client id and tokens. All device data is fetched live from the
/// cloud API on every command.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    pub account: Option<Account>,
    pub token: Option<Token>,
    pub client: Option<Client>,
    pub server: Option<Server>,
    /// Timezone offset used to interpret plug timer schedule times, in the
    /// same format as `--tz` (e.g. "+08:00", "-05:30" or "480" minutes).
    /// When unset the CLI falls back to the machine's local offset (with a
    /// warning).
    #[serde(default)]
    pub tz: Option<String>,
}

impl Config {
    pub fn default_path() -> Result<PathBuf> {
        let dir = dirs::config_dir()
            .or_else(dirs::home_dir)
            .context("cannot locate config directory")?
            .join("oray-tools");
        Ok(dir.join("config.toml"))
    }

    /// Load the config from the platform default path.
    ///
    /// A fresh install has no file yet, so a missing default path silently
    /// yields [`Config::default()`] — unlike [`load_explicit`], where a
    /// missing file is an error, because there the path was named by the
    /// user.
    pub fn load() -> Result<(Config, PathBuf)> {
        load_at(Self::default_path()?, false)
    }

    /// Load the config from a path given explicitly (`--config`).
    ///
    /// A missing file is an **error naming the path**: a typo'd `--config`
    /// (`--config /hme/.../config.toml`) used to be accepted silently, so
    /// the CLI ran account-less and the next `auth login` wrote credentials
    /// to the typo'd location, far away from the real config.
    pub fn load_explicit(path: &std::path::Path) -> Result<(Config, PathBuf)> {
        load_at(path.to_path_buf(), true)
    }

    /// Persist the config atomically with owner-only permissions.
    ///
    /// The file carries credentials (`password_md5`, `refresh_token`), so it
    /// is never written in place: the bytes go to a `0600` temporary file in
    /// the same directory, which is then `rename`d over the target. The
    /// rename is atomic, so a reader (or a process killed mid-save) sees
    /// either the old file or the complete new one — never a truncated one —
    /// and the mode of the temporary file becomes the mode of the config,
    /// tightening an older world-readable file along the way. Any failure
    /// removes the temporary file before the error propagates.
    pub fn save(&self, path: &PathBuf) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("create dir {}", dir.display()))?;
        }
        let raw = toml::to_string_pretty(self).context("serialize config")?;
        let tmp = temp_path(path);
        // A stale temporary file from a recycled pid could carry looser
        // permissions than `mode` grants on an already existing file.
        let _ = std::fs::remove_file(&tmp);
        let saved = write_temp(&tmp, &raw).and_then(|()| {
            std::fs::rename(&tmp, path)
                .with_context(|| format!("replace config {}", path.display()))
        });
        if saved.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        saved
    }

    pub fn server(&self) -> Server {
        self.server.clone().unwrap_or_default().normalized()
    }
}

/// Read the config at `path`.
///
/// `explicit` records where the path came from: `true` for a path the user
/// named on the command line (a missing file is then an error — see
/// [`Config::load_explicit`]), `false` for the platform default, where a
/// fresh install simply has no file yet and defaults apply.
fn load_at(path: PathBuf, explicit: bool) -> Result<(Config, PathBuf)> {
    if !path.exists() {
        if explicit {
            bail!(
                "config file not found: {} (given via --config; fix the path or create the file)",
                path.display()
            );
        }
        return Ok((Config::default(), path));
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("read config {}", path.display()))?;
    let cfg: Config =
        toml::from_str(&raw).with_context(|| format!("parse config {}", path.display()))?;
    Ok((cfg, path))
}

/// Sibling temporary file used by [`Config::save`]: the target name plus the
/// pid, so concurrent writers cannot pick the same name.
fn temp_path(path: &std::path::Path) -> PathBuf {
    let mut name = match path.file_name() {
        Some(n) => n.to_os_string(),
        None => std::ffi::OsString::from("config.toml"),
    };
    name.push(format!(".{}.tmp", std::process::id()));
    path.with_file_name(name)
}

/// Write the serialized config to the temporary file, `0600` on Unix (the
/// file holds credentials; a non-Unix build keeps the platform default).
fn write_temp(tmp: &std::path::Path, raw: &str) -> Result<()> {
    use std::io::Write as _;

    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts
        .open(tmp)
        .with_context(|| format!("write config {}", tmp.display()))?;
    file.write_all(raw.as_bytes())
        .with_context(|| format!("write config {}", tmp.display()))?;
    // Make the data durable before the rename publishes it.
    file.sync_all()
        .with_context(|| format!("write config {}", tmp.display()))?;
    Ok(())
}

impl Default for Server {
    fn default() -> Self {
        Server {
            api_base: DEFAULT_API_BASE.to_string(),
            slapi_base: DEFAULT_SLAPI_BASE.to_string(),
            shield_base: DEFAULT_SHIELD_BASE.to_string(),
        }
    }
}

impl Server {
    pub fn normalized(self) -> Server {
        Server {
            api_base: if self.api_base.is_empty() {
                DEFAULT_API_BASE.to_string()
            } else {
                self.api_base.trim_end_matches('/').to_string()
            },
            slapi_base: if self.slapi_base.is_empty() {
                DEFAULT_SLAPI_BASE.to_string()
            } else {
                self.slapi_base.trim_end_matches('/').to_string()
            },
            shield_base: if self.shield_base.is_empty() {
                DEFAULT_SHIELD_BASE.to_string()
            } else {
                self.shield_base.trim_end_matches('/').to_string()
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_plug_sections_are_ignored() {
        let raw = r#"
[account]
account = "demo"
password_md5 = "abc"

[token]
access_token = "a"
refresh_token = "b"
refresh_expires = 0

[client]
clientid = "uuid"

[plugs.main]
sn = "100000000001"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.account.unwrap().account, "demo");
        assert!(cfg.token.is_some());
    }

    #[test]
    fn server_defaults() {
        let s = Server::default().normalized();
        assert_eq!(s.api_base, DEFAULT_API_BASE);
        assert_eq!(s.slapi_base, DEFAULT_SLAPI_BASE);
        assert_eq!(s.shield_base, DEFAULT_SHIELD_BASE);
    }

    /// A config written before `shield_base` existed still parses, and the
    /// missing value falls back to the default shield service.
    #[test]
    fn legacy_server_without_shield_base() {
        let parsed: Server = toml::from_str(
            r#"
api_base = "https://api.example.com"
slapi_base = "https://slapi.example.net"
"#,
        )
        .unwrap();
        assert!(parsed.shield_base.is_empty(), "field is absent in the file");
        let s = parsed.normalized();
        assert_eq!(s.api_base, "https://api.example.com");
        assert_eq!(s.shield_base, DEFAULT_SHIELD_BASE);
    }

    #[test]
    fn server_trailing_slash_normalized() {
        let s = Server {
            api_base: "https://api.example.com/".into(),
            slapi_base: "".into(),
            shield_base: "https://shield.example.com/".into(),
        }
        .normalized();
        assert_eq!(s.api_base, "https://api.example.com");
        assert_eq!(s.slapi_base, DEFAULT_SLAPI_BASE);
        assert_eq!(s.shield_base, "https://shield.example.com");
    }

    /// The config holds credentials: `save` must create it `0600` (not the
    /// `0644` a plain write would get under umask 022), the bytes must load
    /// back through `Config::load_explicit`, a second save must replace the
    /// existing file (the `rename` path), and no temporary file may be left
    /// behind.
    #[cfg(unix)]
    #[test]
    fn save_is_private_reloadable_and_replaces_atomically() {
        use std::os::unix::fs::PermissionsExt;

        let dir =
            std::env::temp_dir().join(format!("oray-tools-config-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let _ = std::fs::remove_file(&path);

        let cfg = Config {
            account: Some(Account {
                account: "demo".into(),
                password_md5: "abc".into(),
            }),
            token: Some(Token {
                access_token: "at".into(),
                refresh_token: "rt".into(),
                refresh_expires: 123,
            }),
            client: Some(Client {
                clientid: "uuid".into(),
            }),
            ..Default::default()
        };

        cfg.save(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o7777, 0o600, "fresh config must be owner-only");

        let (loaded, loaded_path) = Config::load_explicit(&path).unwrap();
        assert_eq!(loaded_path, path);
        assert_eq!(loaded.account.as_ref().unwrap().account, "demo");
        assert_eq!(loaded.token.as_ref().unwrap().refresh_expires, 123);
        assert_eq!(loaded.client.as_ref().unwrap().clientid, "uuid");

        // Saving again replaces the old file through the rename path.
        let mut second = cfg.clone();
        second.token.as_mut().unwrap().refresh_expires = 456;
        second.save(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o7777, 0o600, "overwritten config stays owner-only");
        let (reloaded, _) = Config::load_explicit(&path).unwrap();
        assert_eq!(reloaded.token.unwrap().refresh_expires, 456);

        // The temporary file never survives a successful save.
        let leftovers: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "leftover temp files: {leftovers:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A typo'd `--config` must fail loudly, naming the path — it used to be
    /// accepted silently, running the CLI account-less and pointing the next
    /// `auth login`'s save at the typo'd location.
    #[test]
    fn explicit_missing_config_is_an_error_naming_the_path() {
        let missing = std::env::temp_dir().join(format!(
            "oray-tools-explicit-missing-config-{}.toml",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&missing);
        let err = Config::load_explicit(&missing).unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("not found"), "{text}");
        assert!(
            text.contains(&missing.display().to_string()),
            "error must name the path: {text}"
        );
    }

    /// The platform default path keeps defaulting silently: a fresh install
    /// simply has no config file yet.
    #[test]
    fn missing_default_config_falls_back_to_defaults() {
        let missing = std::env::temp_dir().join(format!(
            "oray-tools-default-missing-config-{}.toml",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&missing);
        let (cfg, path) = load_at(missing.clone(), false).unwrap();
        assert_eq!(path, missing);
        // The same value a never-configured install starts from.
        assert_eq!(
            serde_json::to_value(&cfg).unwrap(),
            serde_json::to_value(Config::default()).unwrap()
        );
    }

    /// The explicit branch only errors while the file is missing: an
    /// existing `--config` path loads like before.
    #[test]
    fn explicit_existing_config_loads() {
        let dir = std::env::temp_dir().join(format!(
            "oray-tools-explicit-present-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        Config {
            account: Some(Account {
                account: "demo".into(),
                password_md5: "abc".into(),
            }),
            ..Default::default()
        }
        .save(&path)
        .unwrap();
        let (cfg, loaded) = Config::load_explicit(&path).unwrap();
        assert_eq!(loaded, path);
        assert_eq!(cfg.account.unwrap().account, "demo");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
