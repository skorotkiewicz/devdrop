use super::util::{fnv_bytes, now_secs, sql_string};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn db_path(root: &Path) -> PathBuf {
    root.join(".devdrop/devdrop.sqlite")
}

pub fn upsert_user(root: &Path, user: &str) -> Result<(), String> {
    run_sql(
        &db_path(root),
        &format!(
            "INSERT OR REPLACE INTO users (id, workspace_id, name, logged_in_at) VALUES ({}, 'local', {}, {});\n",
            sql_string(user),
            sql_string(user),
            now_secs()
        ),
    )
}

pub fn current_user(root: &Path) -> Result<Option<String>, String> {
    let db = db_path(root);
    if !db.exists() {
        return Ok(None);
    }
    query_one(
        &db,
        "SELECT id FROM users WHERE workspace_id='local' ORDER BY logged_in_at DESC LIMIT 1;",
    )
}

pub fn device_count(root: &Path) -> Result<usize, String> {
    let db = db_path(root);
    if !db.exists() {
        return Ok(0);
    }
    let count = query_one(
        &db,
        "SELECT count(*) FROM devices WHERE workspace_id='local';",
    )?
    .unwrap_or_else(|| "0".to_string());
    count
        .parse()
        .map_err(|err| format!("parse device count: {err}"))
}

pub fn init_db(root: &Path) -> Result<(), String> {
    let db = db_path(root);
    let schema = "
CREATE TABLE IF NOT EXISTS nodes (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL,
  parent_id TEXT,
  path TEXT NOT NULL,
  kind TEXT NOT NULL,
  mode INTEGER,
  size INTEGER,
  content_hash TEXT,
  local_state TEXT NOT NULL,
  remote_manifest_id TEXT,
  local_mtime INTEGER,
  remote_mtime INTEGER,
  deleted_at INTEGER,
  UNIQUE(workspace_id, path)
);
CREATE TABLE IF NOT EXISTS blobs (
  hash TEXT PRIMARY KEY,
  size INTEGER NOT NULL,
  local_path TEXT,
  present INTEGER NOT NULL DEFAULT 0,
  ref_count INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS file_versions (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL,
  path TEXT NOT NULL,
  content_hash TEXT NOT NULL,
  size INTEGER NOT NULL,
  local_path TEXT NOT NULL,
  seen_at INTEGER NOT NULL,
  UNIQUE(workspace_id, path, content_hash)
);
CREATE TABLE IF NOT EXISTS tombstones (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL,
  path TEXT NOT NULL,
  content_hash TEXT,
  deleted_at INTEGER NOT NULL,
  UNIQUE(workspace_id, path, deleted_at)
);
CREATE TABLE IF NOT EXISTS users (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL,
  name TEXT NOT NULL,
  logged_in_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS devices (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL,
  user_id TEXT NOT NULL,
  name TEXT NOT NULL,
  os TEXT NOT NULL,
  arch TEXT NOT NULL,
  trust_level TEXT NOT NULL,
  last_seen_at INTEGER NOT NULL,
  public_key TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS sync_rules (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL,
  pattern TEXT NOT NULL,
  action TEXT NOT NULL,
  priority INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS repo_status (
  node_id TEXT PRIMARY KEY,
  remote_url TEXT,
  branch TEXT,
  head TEXT,
  upstream TEXT,
  ahead INTEGER,
  behind INTEGER,
  dirty INTEGER,
  last_fetch_at INTEGER
);
CREATE TABLE IF NOT EXISTS operations (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL,
  op_type TEXT NOT NULL,
  path TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS secrets (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL,
  path TEXT NOT NULL,
  scope TEXT NOT NULL,
  encrypted_path TEXT NOT NULL,
  materialized INTEGER NOT NULL DEFAULT 0,
  updated_at INTEGER NOT NULL,
  UNIQUE(workspace_id, path, scope)
);
CREATE TABLE IF NOT EXISTS agents (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL,
  repo_path TEXT NOT NULL,
  overlay_path TEXT NOT NULL,
  write_scope TEXT NOT NULL,
  secret_scope TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
";
    run_sql(&db, schema)
}

pub fn run_sql(db: &Path, sql: &str) -> Result<(), String> {
    let mut child = Command::new("sqlite3")
        .arg("-batch")
        .arg(db)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("run sqlite3: {err}"))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "open sqlite3 stdin".to_string())?;
    stdin
        .write_all(sql.as_bytes())
        .map_err(|err| format!("write sqlite3 stdin: {err}"))?;
    drop(stdin);

    let output = child
        .wait_with_output()
        .map_err(|err| format!("wait sqlite3: {err}"))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

pub fn query_one(db: &Path, sql: &str) -> Result<Option<String>, String> {
    let output = Command::new("sqlite3")
        .arg("-batch")
        .arg("-noheader")
        .arg(db)
        .arg(sql)
        .output()
        .map_err(|err| format!("run sqlite3: {err}"))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }

    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text.lines().next().map(str::to_string))
}

pub fn query_lines(db: &Path, sql: &str) -> Result<Vec<String>, String> {
    let output = Command::new("sqlite3")
        .arg("-batch")
        .arg("-noheader")
        .arg(db)
        .arg(sql)
        .output()
        .map_err(|err| format!("run sqlite3: {err}"))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .collect())
}

pub fn log_operation(
    root: &Path,
    op_type: &str,
    path: &str,
    payload: &str,
    status: &str,
) -> Result<(), String> {
    let db = db_path(root);
    if !db.exists() {
        return Ok(());
    }

    let now = now_secs();
    run_sql(&db, &operation_sql(op_type, path, payload, status, now))
}

pub fn operation_sql(op_type: &str, path: &str, payload: &str, status: &str, now: i64) -> String {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let id = format!(
        "op_{unique}_{:016x}",
        fnv_bytes(format!("{op_type}:{path}:{payload}").as_bytes())
    );
    format!(
        "INSERT INTO operations (id, workspace_id, op_type, path, payload_json, status, created_at, updated_at) VALUES ({}, 'local', {}, {}, {}, {}, {}, {});\n",
        sql_string(&id),
        sql_string(op_type),
        sql_string(path),
        sql_string(payload),
        sql_string(status),
        now,
        now
    )
}
