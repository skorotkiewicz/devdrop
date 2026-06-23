use super::fs_util::{display_path, find_workspace_root};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Deserialize, Serialize, Default)]
pub struct Config {
    #[serde(default, skip_serializing_if = "WorkspaceConfig::is_default")]
    pub workspace: WorkspaceConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<RemoteConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pins: Vec<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub secrets: HashMap<String, SecretScope>,
}

#[derive(Debug, Deserialize, Serialize, Default)]
pub struct WorkspaceConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

impl WorkspaceConfig {
    fn is_default(&self) -> bool {
        self.path.is_none()
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RemoteConfig {
    pub url: String,
    #[serde(default)]
    pub auto_sync: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SecretScope {
    #[serde(default = "default_scope")]
    pub scope: String,
    #[serde(default)]
    pub env_vars: Vec<String>,
}

impl Default for SecretScope {
    fn default() -> Self {
        Self {
            scope: default_scope(),
            env_vars: Vec::new(),
        }
    }
}

const CONFIG_FILE: &str = ".devdrop.toml";

fn default_scope() -> String {
    "dev".to_string()
}

pub fn config_path(root: &std::path::Path) -> PathBuf {
    root.join(CONFIG_FILE)
}

pub fn load_config(root: &std::path::Path) -> Result<Config, String> {
    let path = config_path(root);
    if !path.exists() {
        return Ok(Config::default());
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|err| format!("read {}: {err}", display_path(&path)))?;
    toml::from_str(&text).map_err(|err| format!("parse {}: {err}", display_path(&path)))
}

pub fn save_config(root: &std::path::Path, config: &Config) -> Result<(), String> {
    let path = config_path(root);
    let text = toml::to_string_pretty(config).map_err(|err| format!("serialize config: {err}"))?;
    std::fs::write(&path, text).map_err(|err| format!("write {}: {err}", display_path(&path)))
}

pub fn find_config_root(start: &std::path::Path) -> Option<PathBuf> {
    let mut current = if start.is_dir() {
        start
    } else {
        start.parent()?
    };
    loop {
        if current.join(CONFIG_FILE).exists() {
            return Some(current.to_path_buf());
        }
        current = current.parent()?;
    }
}

pub fn get_or_create_config() -> Result<(PathBuf, Config), String> {
    let cwd = std::env::current_dir().map_err(|err| format!("cwd: {err}"))?;
    let root = find_config_root(&cwd)
        .or_else(|| find_workspace_root(&cwd))
        .unwrap_or(cwd);
    Ok((root.clone(), load_config(&root)?))
}

pub fn configured_remote_url(root: &std::path::Path) -> Result<Option<String>, String> {
    Ok(load_config(root)?.remote.map(|remote| remote.url))
}

pub fn configured_secret_scope(
    root: &std::path::Path,
    name: &str,
) -> Result<Option<String>, String> {
    Ok(load_config(root)?
        .secrets
        .get(name)
        .map(|scope| scope.scope.clone()))
}

pub fn set_remote_url(root: &std::path::Path, url: &str, auto_sync: bool) -> Result<(), String> {
    let mut config = load_config(root)?;
    config.remote = Some(RemoteConfig {
        url: url.to_string(),
        auto_sync,
    });
    save_config(root, &config)
}

pub fn cmd_config_get(args: &[String]) -> Result<(), String> {
    let (root, config) = get_or_create_config()?;
    let _ = root;
    let key = args.get(1).ok_or("usage: devdrop config get <key>")?;
    let value = match key.as_str() {
        "remote.url" => config.remote.as_ref().map(|r| r.url.clone()),
        "remote.auto_sync" => config.remote.as_ref().map(|r| r.auto_sync.to_string()),
        "workspace.path" => config.workspace.path.as_ref().map(|p| display_path(p)),
        "pins" => Some(config.pins.join(",")),
        _ => secret_config_value(&config, key),
    };
    if let Some(v) = value {
        println!("{v}");
    }
    Ok(())
}

pub fn cmd_config_set(args: &[String]) -> Result<(), String> {
    let (root, mut config) = get_or_create_config()?;
    let key = args
        .get(1)
        .ok_or("usage: devdrop config set <key> <value>")?;
    let value = args
        .get(2)
        .ok_or("usage: devdrop config set <key> <value>")?;

    match key.as_str() {
        "remote.url" => {
            config.remote = Some(RemoteConfig {
                url: value.clone(),
                auto_sync: false,
            });
        }
        "remote.auto_sync" => {
            if let Some(remote) = config.remote.as_mut() {
                remote.auto_sync = value.parse().map_err(|_| "auto_sync must be true/false")?;
            } else {
                return Err("set remote.url first".into());
            }
        }
        "workspace.path" => {
            config.workspace.path = Some(PathBuf::from(value));
        }
        "pins" => {
            config.pins = split_list(value);
        }
        _ => {
            if let Some(name) = key
                .strip_prefix("secrets.")
                .and_then(|key| key.strip_suffix(".scope"))
            {
                config.secrets.entry(name.to_string()).or_default().scope = value.clone();
            } else if let Some(name) = key
                .strip_prefix("secrets.")
                .and_then(|key| key.strip_suffix(".env_vars"))
            {
                config.secrets.entry(name.to_string()).or_default().env_vars = split_list(value);
            } else {
                return Err(format!("unknown config key: {key}"));
            }
        }
    }

    save_config(&root, &config)?;
    println!("set {key} = {value}");
    Ok(())
}

fn split_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(String::from)
        .collect()
}

fn secret_config_value(config: &Config, key: &str) -> Option<String> {
    if let Some(name) = key
        .strip_prefix("secrets.")
        .and_then(|key| key.strip_suffix(".scope"))
    {
        return config.secrets.get(name).map(|scope| scope.scope.clone());
    }

    if let Some(name) = key
        .strip_prefix("secrets.")
        .and_then(|key| key.strip_suffix(".env_vars"))
    {
        return config
            .secrets
            .get(name)
            .map(|scope| scope.env_vars.join(","));
    }

    config
        .secrets
        .get(key)
        .map(|scope| format!("scope={}, env_vars={:?}", scope.scope, scope.env_vars))
}

pub fn cmd_remote_add(args: &[String]) -> Result<(), String> {
    let (root, _) = get_or_create_config()?;
    let url = args.get(1).ok_or("usage: devdrop remote add <url>")?;
    set_remote_url(&root, url, true)?;
    println!("remote added: {url}");
    Ok(())
}

pub fn cmd_remote_ls(args: &[String]) -> Result<(), String> {
    let _ = args;
    let (root, config) = get_or_create_config()?;
    let _ = root;
    if let Some(remote) = config.remote {
        println!("{}  auto_sync={}", remote.url, remote.auto_sync);
    } else {
        println!("no remote configured");
    }
    Ok(())
}
