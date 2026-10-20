use std::{
    collections::HashMap,
    env::{self, VarError},
    path::PathBuf,
};

use crate::domain::{ChannelId, GuildId};

const DEFAULT_DATABASE_PATH: &str = "data/notbot.sqlite";
const DEFAULT_CONTENT_DIR: &str = "data/content";

pub struct Config {
    discord_token: String,
    database_path: PathBuf,
    content_dir: PathBuf,
    shop_channels: HashMap<GuildId, ChannelId>,
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        let discord_token =
            optional_value("DISCORD_TOKEN")?.ok_or(ConfigError::Missing("DISCORD_TOKEN"))?;
        if discord_token.trim().is_empty() {
            return Err(ConfigError::Invalid("DISCORD_TOKEN"));
        }

        let database_path =
            optional_value("DATABASE_PATH")?.unwrap_or_else(|| DEFAULT_DATABASE_PATH.to_owned());
        let content_dir =
            optional_value("CONTENT_DIR")?.unwrap_or_else(|| DEFAULT_CONTENT_DIR.to_owned());
        if database_path.trim().is_empty() {
            return Err(ConfigError::Invalid("DATABASE_PATH"));
        }
        if content_dir.trim().is_empty() {
            return Err(ConfigError::Invalid("CONTENT_DIR"));
        }

        let raw_channels = optional_value("SHOP_CHANNELS")?.unwrap_or_default();
        let mut shop_channels = HashMap::new();
        if !raw_channels.trim().is_empty() {
            for mapping in raw_channels.split(',') {
                let Some((guild_id, channel_id)) = mapping.split_once('=') else {
                    return Err(ConfigError::Invalid("SHOP_CHANNELS"));
                };
                let guild_id = guild_id
                    .trim()
                    .parse()
                    .map_err(|_| ConfigError::Invalid("SHOP_CHANNELS"))?;
                let channel_id = channel_id
                    .trim()
                    .parse()
                    .map_err(|_| ConfigError::Invalid("SHOP_CHANNELS"))?;
                if shop_channels.insert(guild_id, channel_id).is_some() {
                    return Err(ConfigError::Invalid("SHOP_CHANNELS"));
                }
            }
        }

        Ok(Self {
            discord_token,
            database_path: PathBuf::from(database_path),
            content_dir: PathBuf::from(content_dir),
            shop_channels,
        })
    }

    pub fn discord_token(&self) -> &str {
        &self.discord_token
    }

    pub fn database_path(&self) -> &PathBuf {
        &self.database_path
    }

    pub fn content_dir(&self) -> &PathBuf {
        &self.content_dir
    }

    pub fn shop_channels(&self) -> &HashMap<GuildId, ChannelId> {
        &self.shop_channels
    }
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("環境変数 {0} が設定されていません")]
    Missing(&'static str),
    #[error("環境変数 {0} の値が不正です")]
    Invalid(&'static str),
}

fn optional_value(name: &'static str) -> Result<Option<String>, ConfigError> {
    match env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(VarError::NotPresent) => Ok(None),
        Err(VarError::NotUnicode(_)) => Err(ConfigError::Invalid(name)),
    }
}
