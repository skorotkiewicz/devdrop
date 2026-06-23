use crate::commands::{flag_value, required_path};
use crate::crypto::{openssl_crypt, openssl_decrypt_to_string};
use crate::db::{db_path, init_db, log_operation, query_lines, query_one, run_sql};
use crate::fs_util::{display_path, find_workspace_root, pin_path, rel_or_dot, require_dir};
use crate::index::parent_rel;
use crate::util::{fnv_bytes, json_string, now_secs, sql_string};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn cmd_secret(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("add") => {
            let path = required_path(args.get(1), "secret add")?;
            let scope = flag_value(args, "--scope").unwrap_or("dev");
            cmd_secret_add(&path, scope)
        }
        Some("request") => {
            let path = required_path(args.get(1), "secret request")?;
            let scope = flag_value(args, "--scope").unwrap_or("dev");
            cmd_secret_request(&path, scope)
        }
        Some("unlock") => {
            let path = required_path(args.get(1), "secret unlock")?;
            let scope = flag_value(args, "--scope").unwrap_or("dev");
            cmd_secret_unlock(&path, scope)
        }
        Some("lock") => {
            let path = required_path(args.get(1), "secret lock")?;
            let scope = flag_value(args, "--scope").unwrap_or("dev");
            cmd_secret_lock(&path, scope)
        }
        _ => Err("usage: devdrop secret add|request|unlock|lock <path> [--scope <scope>]".into()),
    }
}

pub fn cmd_run(args: &[String]) -> Result<(), String> {
    let repo = flag_value(args, "--repo")
        .map(PathBuf::from)
        .ok_or_else(|| {
            "usage: devdrop run --repo <path> --secret-scope <scope> -- <command>".to_string()
        })?;
    let scope = flag_value(args, "--secret-scope").unwrap_or("dev");
    let split = args.iter().position(|arg| arg == "--").ok_or_else(|| {
        "usage: devdrop run --repo <path> --secret-scope <scope> -- <command>".to_string()
    })?;
    let command = args
        .get(split + 1)
        .ok_or_else(|| "missing command after --".to_string())?;
    let command_args = &args[split + 2..];
    let envs = secret_env_for_repo(&repo, scope)?;
    let mut child = Command::new(command);
    let status = child
        .args(command_args)
        .current_dir(&repo)
        .env_remove("DEVDROP_SECRET_KEY")
        .envs(envs)
        .status()
        .map_err(|err| format!("run command: {err}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("command exited with {status}"))
    }
}

fn cmd_secret_add(path: &Path, scope: &str) -> Result<(), String> {
    require_secret_key()?;
    if !path.is_file() {
        return Err(format!("not a file: {}", display_path(path)));
    }
    let root = find_workspace_root(path)
        .ok_or_else(|| "no workspace found; run `devdrop init <path>`".to_string())?;
    init_db(&root)?;
    fs::create_dir_all(secret_store_path(&root))
        .map_err(|err| format!("create secret vault: {err}"))?;

    let rel = pin_path(&root, path);
    let encrypted_path = secret_cipher_path(&root, &rel, scope);
    openssl_crypt(false, path, &encrypted_path)?;
    upsert_secret(&root, &rel, scope, &encrypted_path, path.exists())?;
    log_operation(
        &root,
        "secret_add",
        &rel,
        &format!("{{\"scope\":{}}}", json_string(scope)),
        "done",
    )?;
    println!("secret added: {rel} scope={scope}");
    Ok(())
}

fn cmd_secret_unlock(path: &Path, scope: &str) -> Result<(), String> {
    require_secret_key()?;
    let root = find_workspace_root(path)
        .ok_or_else(|| "no workspace found; run `devdrop init <path>`".to_string())?;
    let rel = pin_path(&root, path);
    let secret = lookup_secret(&root, &rel, scope)?;
    openssl_crypt(true, &secret.encrypted_path, path)?;
    upsert_secret(&root, &rel, scope, &secret.encrypted_path, true)?;
    log_operation(
        &root,
        "secret_unlock",
        &rel,
        &format!("{{\"scope\":{}}}", json_string(scope)),
        "done",
    )?;
    println!("secret unlocked: {rel} scope={scope}");
    Ok(())
}

fn cmd_secret_request(path: &Path, scope: &str) -> Result<(), String> {
    require_secret_key()?;
    let root = find_workspace_root(path)
        .ok_or_else(|| "no workspace found; run `devdrop init <path>`".to_string())?;
    let rel = pin_path(&root, path);
    let secret = lookup_secret(&root, &rel, scope)?;
    let plaintext = openssl_decrypt_to_string(&secret.encrypted_path)?;
    print!("{plaintext}");
    log_operation(
        &root,
        "secret_request",
        &rel,
        &format!("{{\"scope\":{}}}", json_string(scope)),
        "done",
    )?;
    Ok(())
}

fn cmd_secret_lock(path: &Path, scope: &str) -> Result<(), String> {
    let root = find_workspace_root(path)
        .ok_or_else(|| "no workspace found; run `devdrop init <path>`".to_string())?;
    let rel = pin_path(&root, path);
    let secret = lookup_secret(&root, &rel, scope)?;
    if path.exists() {
        fs::remove_file(path).map_err(|err| format!("lock {}: {err}", display_path(path)))?;
    }
    upsert_secret(&root, &rel, scope, &secret.encrypted_path, false)?;
    log_operation(
        &root,
        "secret_lock",
        &rel,
        &format!("{{\"scope\":{}}}", json_string(scope)),
        "done",
    )?;
    println!("secret locked: {rel} scope={scope}");
    Ok(())
}

struct SecretRow {
    encrypted_path: PathBuf,
}

pub fn secret_store_path(root: &Path) -> PathBuf {
    root.join(".devdrop/secrets")
}

pub fn secret_cipher_path(root: &Path, rel: &str, scope: &str) -> PathBuf {
    secret_store_path(root).join(format!(
        "secret_{:016x}_{:016x}.enc",
        fnv_bytes(rel.as_bytes()),
        fnv_bytes(scope.as_bytes())
    ))
}

fn require_secret_key() -> Result<(), String> {
    env::var("DEVDROP_SECRET_KEY")
        .map(|_| ())
        .map_err(|_| "DEVDROP_SECRET_KEY is required for secret commands".to_string())
}

pub fn upsert_secret(
    root: &Path,
    rel: &str,
    scope: &str,
    encrypted_path: &Path,
    materialized: bool,
) -> Result<(), String> {
    let db = db_path(root);
    let now = now_secs();
    let id = format!(
        "secret_{:016x}_{:016x}",
        fnv_bytes(rel.as_bytes()),
        fnv_bytes(scope.as_bytes())
    );
    let sql = format!(
        "INSERT OR REPLACE INTO secrets (id, workspace_id, path, scope, encrypted_path, materialized, updated_at) VALUES ({}, 'local', {}, {}, {}, {}, {});\n",
        sql_string(&id),
        sql_string(rel),
        sql_string(scope),
        sql_string(&encrypted_path.to_string_lossy()),
        i32::from(materialized),
        now
    );
    run_sql(&db, &sql)
}

fn lookup_secret(root: &Path, rel: &str, scope: &str) -> Result<SecretRow, String> {
    init_db(root)?;
    let db = db_path(root);
    if !db.exists() {
        return Err(format!(
            "no local index; run `devdrop init {}` first",
            display_path(root)
        ));
    }

    let sql = format!(
        "SELECT encrypted_path FROM secrets WHERE workspace_id='local' AND path={} AND scope={} LIMIT 1;",
        sql_string(rel),
        sql_string(scope)
    );
    let encrypted_path =
        query_one(&db, &sql)?.ok_or_else(|| format!("secret not found: {rel} scope={scope}"))?;
    Ok(SecretRow {
        encrypted_path: PathBuf::from(encrypted_path),
    })
}

pub fn locked_secret_count(root: &Path) -> Result<usize, String> {
    let db = db_path(root);
    if !db.exists() {
        return Ok(0);
    }
    let count = query_one(
        &db,
        "SELECT count(*) FROM secrets WHERE workspace_id='local' AND materialized=0;",
    )?
    .unwrap_or_else(|| "0".to_string());
    count
        .parse()
        .map_err(|err| format!("parse locked secret count: {err}"))
}

pub fn locked_secrets_in_dir(dir: &Path) -> Result<Vec<String>, String> {
    let Some(root) = find_workspace_root(dir) else {
        return Ok(Vec::new());
    };
    let db = db_path(&root);
    if !db.exists() {
        return Ok(Vec::new());
    }

    let dir_rel = rel_or_dot(&root, dir);
    let mut names = query_lines(
        &db,
        "SELECT path FROM secrets WHERE workspace_id='local' AND materialized=0 ORDER BY path;",
    )?
    .into_iter()
    .filter(|secret| parent_rel(secret).as_deref() == Some(dir_rel.as_str()))
    .filter_map(|secret| secret.rsplit('/').next().map(str::to_string))
    .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    Ok(names)
}

fn secret_env_for_repo(repo: &Path, scope: &str) -> Result<Vec<(String, String)>, String> {
    require_secret_key()?;
    require_dir(repo)?;
    let root = find_workspace_root(repo)
        .ok_or_else(|| "no workspace found; run `devdrop init <path>`".to_string())?;
    let path = repo.join(".env");
    let rel = pin_path(&root, &path);
    let secret = lookup_secret(&root, &rel, scope)?;
    let plaintext = openssl_decrypt_to_string(&secret.encrypted_path)?;
    parse_env(&plaintext)
}

pub fn parse_env(text: &str) -> Result<Vec<(String, String)>, String> {
    let mut envs = Vec::new();

    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| format!("invalid env line {}: missing =", index + 1))?;
        let key = key.trim();
        if key.is_empty()
            || !key
                .chars()
                .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
        {
            return Err(format!("invalid env key on line {}", index + 1));
        }

        envs.push((key.to_string(), unquote_env_value(value.trim()).to_string()));
    }

    Ok(envs)
}

fn unquote_env_value(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })
        .unwrap_or(value)
}
