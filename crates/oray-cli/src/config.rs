use anyhow::{Context, Result};
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

    pub fn load(path: Option<&PathBuf>) -> Result<(Config, PathBuf)> {
        let p = path.cloned().unwrap_or(Self::default_path()?);
        if !p.exists() {
            return Ok((Config::default(), p));
        }
        let raw =
            std::fs::read_to_string(&p).with_context(|| format!("read config {}", p.display()))?;
        let cfg: Config =
            toml::from_str(&raw).with_context(|| format!("parse config {}", p.display()))?;
        Ok((cfg, p))
    }

    pub fn save(&self, path: &PathBuf) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("create dir {}", dir.display()))?;
        }
        let raw = toml::to_string_pretty(self).context("serialize config")?;
        std::fs::write(path, raw).with_context(|| format!("write config {}", path.display()))?;
        Ok(())
    }

    pub fn server(&self) -> Server {
        self.server.clone().unwrap_or_default().normalized()
    }
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
}
