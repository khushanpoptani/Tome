use std::{env, net::IpAddr, path::PathBuf, str::FromStr};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const DEFAULT_PORT: u16 = 7331;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkMode {
    Loopback,
    Lan,
    Tailscale,
}

impl FromStr for NetworkMode {
    type Err = ConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "loopback" => Ok(Self::Loopback),
            "lan" => Ok(Self::Lan),
            "tailscale" => Ok(Self::Tailscale),
            _ => Err(ConfigError::InvalidMode(value.to_owned())),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub bind_address: IpAddr,
    pub port: u16,
    pub network_mode: NetworkMode,
    pub allow_public_bind: bool,
    pub database_path: PathBuf,
    pub temp_directory: PathBuf,
    pub retention_days: u32,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("TOME_NETWORK_MODE must be loopback, lan, or tailscale; received {0:?}")]
    InvalidMode(String),
    #[error("{name} has invalid value {value:?}: {reason}")]
    InvalidValue {
        name: &'static str,
        value: String,
        reason: &'static str,
    },
    #[error("loopback mode requires a loopback bind address")]
    LoopbackAddressRequired,
    #[error("LAN mode requires a private, link-local, or loopback bind address")]
    PrivateAddressRequired,
    #[error("Tailscale mode requires an address in 100.64.0.0/10 or fd7a:115c:a1e0::/48")]
    TailscaleAddressRequired,
    #[error("wildcard or public bind addresses require TOME_ALLOW_PUBLIC_BIND=true")]
    PublicBindNotAllowed,
}

impl Config {
    /// Loads configuration from the process environment and validates it.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when a value cannot be parsed or the network mode and bind address
    /// would create an unsafe or inconsistent configuration.
    pub fn from_env() -> Result<Self, ConfigError> {
        let network_mode = env_value("TOME_NETWORK_MODE")
            .unwrap_or_else(|| "loopback".to_owned())
            .parse()?;
        let bind_address = parse_env("TOME_BIND_ADDRESS", "127.0.0.1")?;
        let port = parse_env("TOME_PORT", &DEFAULT_PORT.to_string())?;
        let allow_public_bind = parse_bool_env("TOME_ALLOW_PUBLIC_BIND", false)?;
        let retention_days = parse_env("TOME_JOB_RETENTION_DAYS", "30")?;
        let config = Self {
            bind_address,
            port,
            network_mode,
            allow_public_bind,
            database_path: PathBuf::from(
                env_value("TOME_DATABASE_PATH").unwrap_or_else(|| "tome.sqlite3".to_owned()),
            ),
            temp_directory: PathBuf::from(
                env_value("TOME_TEMP_DIRECTORY").unwrap_or_else(|| "tome-temp".to_owned()),
            ),
            retention_days,
        };
        config.validate()?;
        Ok(config)
    }

    /// Checks port, retention, bind-address safety, and network-mode consistency.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when the configuration is invalid or publicly exposed without the
    /// explicit override.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.port == 0 {
            return Err(ConfigError::InvalidValue {
                name: "TOME_PORT",
                value: "0".to_owned(),
                reason: "port must be between 1 and 65535",
            });
        }
        if self.retention_days == 0 {
            return Err(ConfigError::InvalidValue {
                name: "TOME_JOB_RETENTION_DAYS",
                value: "0".to_owned(),
                reason: "retention must be at least one day",
            });
        }

        let unsafe_address = self.bind_address.is_unspecified()
            || !(self.bind_address.is_loopback()
                || is_private_or_link_local(self.bind_address)
                || is_tailscale(self.bind_address));
        if unsafe_address && !self.allow_public_bind {
            return Err(ConfigError::PublicBindNotAllowed);
        }

        match self.network_mode {
            NetworkMode::Loopback if !self.bind_address.is_loopback() => {
                Err(ConfigError::LoopbackAddressRequired)
            }
            NetworkMode::Lan
                if !(self.bind_address.is_loopback()
                    || is_private_or_link_local(self.bind_address)
                    || self.allow_public_bind) =>
            {
                Err(ConfigError::PrivateAddressRequired)
            }
            NetworkMode::Tailscale if !is_tailscale(self.bind_address) => {
                Err(ConfigError::TailscaleAddressRequired)
            }
            _ => Ok(()),
        }
    }
}

fn env_value(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.trim().is_empty())
}

fn parse_env<T>(name: &'static str, default: &str) -> Result<T, ConfigError>
where
    T: FromStr,
{
    let value = env_value(name).unwrap_or_else(|| default.to_owned());
    value.parse().map_err(|_| ConfigError::InvalidValue {
        name,
        value,
        reason: "could not parse value",
    })
}

fn parse_bool_env(name: &'static str, default: bool) -> Result<bool, ConfigError> {
    let Some(value) = env_value(name) else {
        return Ok(default);
    };
    match value.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" => Ok(true),
        "false" | "0" | "no" => Ok(false),
        _ => Err(ConfigError::InvalidValue {
            name,
            value,
            reason: "expected true or false",
        }),
    }
}

fn is_private_or_link_local(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => address.is_private() || address.is_link_local(),
        IpAddr::V6(address) => address.is_unique_local() || address.is_unicast_link_local(),
    }
}

fn is_tailscale(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            let octets = address.octets();
            octets[0] == 100 && (64..=127).contains(&octets[1])
        }
        IpAddr::V6(address) => {
            let segments = address.segments();
            segments[0] == 0xfd7a && segments[1] == 0x115c && segments[2] == 0xa1e0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(mode: NetworkMode, address: &str) -> Config {
        Config {
            bind_address: address.parse().unwrap(),
            port: DEFAULT_PORT,
            network_mode: mode,
            allow_public_bind: false,
            database_path: "test.sqlite3".into(),
            temp_directory: "temp".into(),
            retention_days: 30,
        }
    }

    #[test]
    fn defaults_are_loopback_safe() {
        config(NetworkMode::Loopback, "127.0.0.1")
            .validate()
            .unwrap();
    }

    #[test]
    fn wildcard_bind_is_rejected_by_default() {
        assert_eq!(
            config(NetworkMode::Lan, "0.0.0.0").validate(),
            Err(ConfigError::PublicBindNotAllowed)
        );
    }

    #[test]
    fn explicit_override_allows_a_lan_wildcard_bind() {
        let mut config = config(NetworkMode::Lan, "0.0.0.0");
        config.allow_public_bind = true;
        config.validate().unwrap();
    }

    #[test]
    fn modes_must_match_addresses() {
        assert_eq!(
            config(NetworkMode::Loopback, "192.168.1.10").validate(),
            Err(ConfigError::LoopbackAddressRequired)
        );
        assert_eq!(
            config(NetworkMode::Tailscale, "192.168.1.10").validate(),
            Err(ConfigError::TailscaleAddressRequired)
        );
        config(NetworkMode::Tailscale, "100.100.20.30")
            .validate()
            .unwrap();
    }
}
