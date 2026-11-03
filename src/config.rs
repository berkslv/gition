//! Per-repo configuration, read from `.gition.toml` at the repo root.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

pub const FILE_NAME: &str = ".gition.toml";

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub tree_width: u16,
    pub assets_dir: String,
    pub show_all_files: bool,
    pub commit_prefix: String,
    /// Action name -> one key or a list of keys, e.g. `quit = ["q", "ctrl+q"]`.
    pub keys: BTreeMap<String, KeyList>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum KeyList {
    One(String),
    Many(Vec<String>),
}

impl KeyList {
    pub fn to_vec(&self) -> Vec<String> {
        match self {
            KeyList::One(s) => vec![s.clone()],
            KeyList::Many(v) => v.clone(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            tree_width: 30,
            assets_dir: "assets".into(),
            show_all_files: false,
            commit_prefix: "gition:".into(),
            keys: BTreeMap::new(),
        }
    }
}

impl Config {
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join(FILE_NAME);
        if !path.exists() {
            return Ok(Self::default());
        }
        Self::parse(&std::fs::read_to_string(&path)?)
    }

    pub fn parse(text: &str) -> Result<Self> {
        toml::from_str(text).with_context(|| format!("invalid {FILE_NAME}"))
    }
}
