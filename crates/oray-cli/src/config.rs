use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

pub const DEFAULT_API_BASE: &str = "https://api-std.sunlogin.oray.com";
pub const DEFAULT_SLAPI_BASE: &str = "https://slapi.oray.net";

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
pub struct Device {
    pub sn: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Client {
    pub clientid: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Server {
    pub api_base: String,
    pub slapi_base: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    pub account: Option<Account>,
    pub token: Option<Token>,
    pub client: Option<Client>,
    /// Named plug registry: name -> device.
    #[serde(default)]
    pub plugs: HashMap<String, Device>,
    /// Legacy single-device section, migrated into `plugs` on load.
    #[serde(default, rename = "device", skip_serializing)]
    pub legacy_device: Option<Device>,
    pub server: Option<Server>,
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
        let raw = std::fs::read_to_string(&p).with_context(|| format!("read config {}", p.display()))?;
        let mut cfg: Config = toml::from_str(&raw).with_context(|| format!("parse config {}", p.display()))?;
        if cfg.plugs.is_empty()
            && let Some(dev) = cfg.legacy_device.take()
        {
            cfg.plugs.insert("default".to_string(), dev);
        }
        Ok((cfg, p))
    }

    pub fn save(&self, path: &PathBuf) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("create dir {}", dir.display()))?;
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
        }
    }
}