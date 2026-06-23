use super::db::{db_path, init_db, log_operation, operation_sql, query_lines, query_one, run_sql};
use super::fs_util::{
    display_path, find_workspace_root, pin_path, rel_or_dot, rel_path, require_dir,
    skip_overlay_component,
};
use super::git::{RepoStatus, is_repo, repo_dirty, repo_status};
use super::remote::fetch_remote_object;
use super::rules::{Action, Rules};
use super::util::{fnv_bytes, hex_decode_string, now_secs, sql_optional, sql_string};
use std::collections::{HashMap, HashSet};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

pub fn object_store_path(root: &Path) -> PathBuf {
    root.join(".devdrop/objects")
}

pub fn tree_signature(root: &Path) -> Result<HashMap<String, String>, String> {
    require_dir(root)?;
    let mut signature = HashMap::new();
    collect_tree_signature(root, root, &mut signature)?;
    Ok(signature)
}

pub fn collect_tree_signature(
    root: &Path,
    current: &Path,
    signature: &mut HashMap<String, String>,
) -> Result<(), String> {
    for entry in
        fs::read_dir(current).map_err(|err| format!("read {}: {err}", display_path(current)))?
    {
        let entry = entry.map_err(|err| format!("read {}: {err}", display_path(current)))?;
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .map_err(|err| format!("relative path: {err}"))?;
        if skip_overlay_component(rel) {
            continue;
        }
        let rel = rel_path(root, &path);
        let file_type = entry
            .file_type()
            .map_err(|err| format!("stat {}: {err}", display_path(&path)))?;
        if file_type.is_dir() {
            signature.insert(rel, "dir".to_string());
            collect_tree_signature(root, &path, signature)?;
        } else if file_type.is_file() {
            let (hash, size) = file_hash(&path)?;
            signature.insert(rel, format!("file:{hash}:{size}"));
        } else {
            signature.insert(rel, "other".to_string());
        }
    }
    Ok(())
}

pub fn mark_node_local(root: &Path, rel: &str) -> Result<(), String> {
    let db = db_path(root);
    if !db.exists() {
        return Ok(());
    }
    run_sql(
        &db,
        &format!(
            "UPDATE nodes SET local_state='local' WHERE workspace_id='local' AND path={};
UPDATE blobs SET present=1 WHERE hash=(SELECT content_hash FROM nodes WHERE workspace_id='local' AND path={});
",
            sql_string(rel),
            sql_string(rel)
        ),
    )
}

pub fn hydrate_from_local_store(path: &Path) -> Result<(), String> {
    let root = find_workspace_root(path).ok_or_else(|| {
        format!(
            "no workspace found; run `devdrop init .`; cannot hydrate {}",
            display_path(path)
        )
    })?;
    let db = db_path(&root);
    if !db.exists() {
        return Err(format!(
            "no local index; run `devdrop sync {}` first",
            display_path(&root)
        ));
    }

    let rel = pin_path(&root, path);
    let sql = format!(
        "SELECT content_hash FROM nodes WHERE workspace_id='local' AND path={} AND content_hash IS NOT NULL LIMIT 1;",
        sql_string(&rel)
    );
    let hash = query_one(&db, &sql)?
        .ok_or_else(|| format!("no indexed blob for {}", display_path(path)))?;
    let object = object_path(&root, &hash);
    if !object.exists() {
        fetch_remote_object(&root, &hash)?;
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("create parent dir: {err}"))?;
    }
    fs::copy(&object, path).map_err(|err| format!("hydrate {}: {err}", display_path(path)))?;
    mark_node_local(&root, &rel)?;
    log_operation(&root, "hydrate", &rel, "{}", "done")?;
    println!("hydrated: {}", display_path(path));
    Ok(())
}

pub struct IndexedEntry {
    pub name: String,
    pub state: String,
    pub kind: String,
}

pub fn indexed_state_count(root: &Path, state: &str) -> Result<usize, String> {
    let db = db_path(root);
    if !db.exists() {
        return Ok(0);
    }
    let count = query_one(
        &db,
        &format!(
            "SELECT count(*) FROM nodes WHERE workspace_id='local' AND local_state={};",
            sql_string(state)
        ),
    )?
    .unwrap_or_else(|| "0".to_string());
    count
        .parse()
        .map_err(|err| format!("parse indexed state count: {err}"))
}

pub fn indexed_entries_in_dir(dir: &Path) -> Result<Vec<IndexedEntry>, String> {
    let Some(root) = find_workspace_root(dir) else {
        return Ok(Vec::new());
    };
    let db = db_path(&root);
    if !db.exists() {
        return Ok(Vec::new());
    }

    let dir_rel = rel_or_dot(&root, dir);
    let mut entries = query_lines(
        &db,
        "SELECT hex(path)||char(9)||hex(local_state)||char(9)||hex(kind) FROM nodes WHERE workspace_id='local' ORDER BY path;",
    )?
    .into_iter()
    .filter_map(|row| indexed_entry_from_row(&row, &dir_rel).transpose())
    .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(entries)
}

pub fn indexed_entry_from_row(row: &str, dir_rel: &str) -> Result<Option<IndexedEntry>, String> {
    let fields = row.split('\t').collect::<Vec<_>>();
    if fields.len() != 3 {
        return Ok(None);
    }

    let path = hex_decode_string(fields[0])?;
    if path == "." || parent_rel(&path).as_deref() != Some(dir_rel) {
        return Ok(None);
    }

    let state = hex_decode_string(fields[1])?;
    if !matches!(
        state.as_str(),
        "remote-only" | "metadata-only" | "secret-locked"
    ) {
        return Ok(None);
    }

    Ok(Some(IndexedEntry {
        name: path.rsplit('/').next().unwrap_or(&path).to_string(),
        state,
        kind: hex_decode_string(fields[2])?,
    }))
}

#[derive(Default)]
pub struct Counts {
    pub entries: usize,
    pub local: usize,
    pub ignored: usize,
    pub metadata_only: usize,
    pub local_only: usize,
    pub remote_only: usize,
    pub secret_locked: usize,
    pub conflicted: usize,
    pub repos: usize,
    pub dirty_repos: usize,
}

pub struct IndexSnapshot {
    pub nodes: Vec<IndexNode>,
    pub blobs: HashMap<String, BlobRow>,
    pub repos: Vec<(String, RepoStatus)>,
}

pub struct IndexNode {
    pub id: String,
    pub parent_id: Option<String>,
    pub path: String,
    pub kind: String,
    pub mode: u32,
    pub size: u64,
    pub content_hash: Option<String>,
    pub local_state: String,
    pub local_mtime: i64,
}

pub struct BlobRow {
    pub hash: String,
    pub size: u64,
    pub local_path: String,
    pub ref_count: usize,
}

pub struct PreviousFile {
    pub path: String,
    pub content_hash: String,
}

pub struct FileVersion {
    pub content_hash: String,
    pub size: u64,
    pub seen_at: i64,
}

pub fn scan_workspace(root: &Path, rules: &Rules) -> Result<Counts, String> {
    let mut counts = Counts::default();

    walk_dirs(root, root, rules, &mut |path, file_type, action| {
        let rel = rel_path(root, path);
        counts.entries += 1;
        if is_conflict_path(&rel) {
            counts.conflicted += 1;
        } else {
            match action {
                Action::Sync => counts.local += 1,
                Action::Ignore => counts.ignored += 1,
                Action::MetadataOnly => counts.metadata_only += 1,
                Action::LocalOnly => counts.local_only += 1,
                Action::HydrateOnAccess => counts.remote_only += 1,
                Action::Secret => counts.secret_locked += 1,
            }
        }

        if file_type.is_dir() && is_repo(path) {
            counts.repos += 1;
            if repo_dirty(path) {
                counts.dirty_repos += 1;
            }
        }

        Ok(file_type.is_dir() && !action.skips_children())
    })?;

    if is_repo(root) {
        counts.repos += 1;
        if repo_dirty(root) {
            counts.dirty_repos += 1;
        }
    }

    Ok(counts)
}

pub fn collect_index(root: &Path, rules: &Rules) -> Result<IndexSnapshot, String> {
    fs::create_dir_all(object_store_path(root))
        .map_err(|err| format!("create object store: {err}"))?;

    let mut nodes = Vec::new();
    let mut blobs = HashMap::new();
    let mut repos = Vec::new();
    let metadata =
        fs::symlink_metadata(root).map_err(|err| format!("stat {}: {err}", display_path(root)))?;

    push_index_node(
        root,
        root,
        ".",
        Action::Sync,
        &metadata,
        &mut nodes,
        &mut blobs,
    )?;

    if is_repo(root) {
        repos.push((".".to_string(), repo_status(root)));
    }

    walk_dirs(root, root, rules, &mut |path, file_type, action| {
        let rel = rel_path(root, path);
        let metadata = fs::symlink_metadata(path)
            .map_err(|err| format!("stat {}: {err}", display_path(path)))?;
        push_index_node(root, path, &rel, action, &metadata, &mut nodes, &mut blobs)?;

        if file_type.is_dir() && is_repo(path) {
            repos.push((rel, repo_status(path)));
        }

        Ok(file_type.is_dir() && !action.skips_children())
    })?;

    Ok(IndexSnapshot {
        nodes,
        blobs,
        repos,
    })
}

pub fn push_index_node(
    root: &Path,
    path: &Path,
    rel: &str,
    action: Action,
    metadata: &fs::Metadata,
    nodes: &mut Vec<IndexNode>,
    blobs: &mut HashMap<String, BlobRow>,
) -> Result<(), String> {
    let content = if metadata.is_file() && matches!(action, Action::Sync | Action::HydrateOnAccess)
    {
        let (hash, size) = file_hash(path)?;
        let object = object_path(root, &hash);
        if !object.exists() {
            fs::copy(path, &object)
                .map_err(|err| format!("store blob {}: {err}", display_path(path)))?;
        }

        blobs
            .entry(hash.clone())
            .and_modify(|blob| blob.ref_count += 1)
            .or_insert_with(|| BlobRow {
                hash: hash.clone(),
                size,
                local_path: object.to_string_lossy().into_owned(),
                ref_count: 1,
            });

        Some(hash)
    } else {
        None
    };

    nodes.push(IndexNode {
        id: node_id(rel),
        parent_id: parent_rel(rel).map(|parent| node_id(&parent)),
        path: rel.to_string(),
        kind: node_kind(path, action, metadata),
        mode: file_mode(metadata),
        size: metadata.len(),
        content_hash: content,
        local_state: local_state_for(rel, action).to_string(),
        local_mtime: modified_secs(metadata),
    });

    Ok(())
}

pub fn carry_indexed_remote_nodes(root: &Path, snapshot: &mut IndexSnapshot) -> Result<(), String> {
    let db = db_path(root);
    if !db.exists() {
        return Ok(());
    }

    let mut present = snapshot
        .nodes
        .iter()
        .map(|node| node.path.clone())
        .collect::<HashSet<_>>();
    for row in query_lines(
        &db,
        "SELECT hex(path)||char(9)||hex(kind)||char(9)||mode||char(9)||size||char(9)||ifnull(hex(content_hash),'')||char(9)||hex(local_state)||char(9)||ifnull(local_mtime,0) FROM nodes WHERE workspace_id='local' AND local_state IN ('remote-only','metadata-only','secret-locked') ORDER BY path;",
    )? {
        let fields = row.split('\t').collect::<Vec<_>>();
        if fields.len() != 7 {
            return Err("bad carried node row".into());
        }
        let path = hex_decode_string(fields[0])?;
        if !present.insert(path.clone()) || root.join(&path).exists() {
            continue;
        }
        snapshot.nodes.push(IndexNode {
            id: node_id(&path),
            parent_id: parent_rel(&path).map(|parent| node_id(&parent)),
            path,
            kind: hex_decode_string(fields[1])?,
            mode: fields[2]
                .parse()
                .map_err(|err| format!("bad carried node mode: {err}"))?,
            size: fields[3]
                .parse()
                .map_err(|err| format!("bad carried node size: {err}"))?,
            content_hash: (!fields[4].is_empty())
                .then(|| hex_decode_string(fields[4]))
                .transpose()?,
            local_state: hex_decode_string(fields[5])?,
            local_mtime: fields[6]
                .parse()
                .map_err(|err| format!("bad carried node mtime: {err}"))?,
        });
    }

    Ok(())
}

pub fn previous_index_files(root: &Path) -> Result<Vec<PreviousFile>, String> {
    let db = db_path(root);
    if !db.exists() {
        return Ok(Vec::new());
    }

    query_lines(
        &db,
        "SELECT hex(path)||char(9)||hex(content_hash) FROM nodes WHERE workspace_id='local' AND content_hash IS NOT NULL;",
    )?
    .into_iter()
    .map(|line| {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 2 {
            return Err("bad previous file row".to_string());
        }
        Ok(PreviousFile {
            path: hex_decode_string(fields[0])?,
            content_hash: hex_decode_string(fields[1])?,
        })
    })
    .collect()
}

pub fn file_version_sql(
    path: &str,
    hash: &str,
    size: u64,
    local_path: &Path,
    seen_at: i64,
) -> String {
    format!(
        "INSERT OR REPLACE INTO file_versions (id, workspace_id, path, content_hash, size, local_path, seen_at) VALUES ({}, 'local', {}, {}, {}, {}, {});\n",
        sql_string(&format!(
            "version_{:016x}_{:016x}",
            fnv_bytes(path.as_bytes()),
            fnv_bytes(hash.as_bytes())
        )),
        sql_string(path),
        sql_string(hash),
        size,
        sql_string(&local_path.to_string_lossy()),
        seen_at
    )
}

pub fn file_versions(root: &Path, rel: &str) -> Result<Vec<FileVersion>, String> {
    init_db(root)?;
    query_lines(
        &db_path(root),
        &format!(
            "SELECT hex(content_hash)||char(9)||size||char(9)||seen_at FROM file_versions WHERE workspace_id='local' AND path={} ORDER BY seen_at DESC;",
            sql_string(rel)
        ),
    )?
    .into_iter()
    .map(|line| {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 3 {
            return Err("bad file version row".to_string());
        }
        Ok(FileVersion {
            content_hash: hex_decode_string(fields[0])?,
            size: fields[1]
                .parse()
                .map_err(|err| format!("bad version size: {err}"))?,
            seen_at: fields[2]
                .parse()
                .map_err(|err| format!("bad version timestamp: {err}"))?,
        })
    })
    .collect()
}

pub fn latest_file_version_hash(root: &Path, rel: &str) -> Result<Option<String>, String> {
    init_db(root)?;
    query_one(
        &db_path(root),
        &format!(
            "SELECT content_hash FROM file_versions WHERE workspace_id='local' AND path={} ORDER BY seen_at DESC LIMIT 1;",
            sql_string(rel)
        ),
    )
}

pub fn local_state_for(rel: &str, action: Action) -> &'static str {
    if is_conflict_path(rel) {
        "conflicted"
    } else {
        action.state_label()
    }
}

pub fn is_conflict_path(rel: &str) -> bool {
    rel.rsplit('/').next().is_some_and(|name| {
        name.contains(".conflict-") || (name.contains(" (conflict from ") && name.contains(')'))
    })
}

pub fn write_index(root: &Path, rules: &Rules, snapshot: &IndexSnapshot) -> Result<(), String> {
    init_db(root)?;
    let db = db_path(root);
    let now = now_secs();
    let previous = previous_index_files(root)?;
    let current_paths = snapshot
        .nodes
        .iter()
        .map(|node| node.path.as_str())
        .collect::<HashSet<_>>();
    let mut sql = String::from(
        "
BEGIN;
DELETE FROM nodes;
DELETE FROM blobs;
DELETE FROM sync_rules;
DELETE FROM repo_status;
",
    );

    for (priority, rule) in rules.rules.iter().enumerate() {
        sql.push_str(&format!(
            "INSERT INTO sync_rules (id, workspace_id, pattern, action, priority) VALUES ({}, 'local', {}, {}, {});\n",
            sql_string(&format!("rule_{priority}")),
            sql_string(&rule.pattern),
            sql_string(rule.action.token()),
            priority
        ));
    }

    for node in &snapshot.nodes {
        sql.push_str(&format!(
            "INSERT OR REPLACE INTO nodes (id, workspace_id, parent_id, path, kind, mode, size, content_hash, local_state, remote_manifest_id, local_mtime, remote_mtime, deleted_at) VALUES ({}, 'local', {}, {}, {}, {}, {}, {}, {}, NULL, {}, NULL, NULL);\n",
            sql_string(&node.id),
            sql_optional(node.parent_id.as_deref()),
            sql_string(&node.path),
            sql_string(&node.kind),
            node.mode,
            node.size,
            sql_optional(node.content_hash.as_deref()),
            sql_string(&node.local_state),
            node.local_mtime
        ));
    }

    for blob in snapshot.blobs.values() {
        sql.push_str(&format!(
            "INSERT OR REPLACE INTO blobs (hash, size, local_path, present, ref_count) VALUES ({}, {}, {}, 1, {});\n",
            sql_string(&blob.hash),
            blob.size,
            sql_string(&blob.local_path),
            blob.ref_count
        ));
    }

    for node in snapshot
        .nodes
        .iter()
        .filter(|node| node.content_hash.is_some())
    {
        let hash = node.content_hash.as_deref().unwrap_or_default();
        sql.push_str(&file_version_sql(
            &node.path,
            hash,
            node.size,
            &object_path(root, hash),
            now,
        ));
    }

    for file in previous
        .iter()
        .filter(|file| !current_paths.contains(file.path.as_str()))
    {
        sql.push_str(&format!(
            "INSERT OR IGNORE INTO tombstones (id, workspace_id, path, content_hash, deleted_at) VALUES ({}, 'local', {}, {}, {});\n",
            sql_string(&format!("tombstone_{:016x}_{}", fnv_bytes(file.path.as_bytes()), now)),
            sql_string(&file.path),
            sql_optional(Some(file.content_hash.as_str())),
            now
        ));
    }

    for (rel, status) in &snapshot.repos {
        sql.push_str(&format!(
            "INSERT OR REPLACE INTO repo_status (node_id, remote_url, branch, head, upstream, ahead, behind, dirty, last_fetch_at) VALUES ({}, {}, {}, {}, {}, {}, {}, {}, {});\n",
            sql_string(&node_id(rel)),
            sql_optional(status.remote_url.as_deref()),
            sql_optional(status.branch.as_deref()),
            sql_optional(status.head.as_deref()),
            sql_optional(status.upstream.as_deref()),
            status.ahead.unwrap_or(0),
            status.behind.unwrap_or(0),
            i32::from(status.dirty),
            now
        ));
    }

    let payload = format!(
        "{{\"nodes\":{},\"blobs\":{},\"repos\":{}}}",
        snapshot.nodes.len(),
        snapshot.blobs.len(),
        snapshot.repos.len()
    );
    sql.push_str(&operation_sql("sync", ".", &payload, "done", now));
    sql.push_str("COMMIT;\n");
    run_sql(&db, &sql)
}

pub fn walk_dirs<F>(root: &Path, path: &Path, rules: &Rules, visit: &mut F) -> Result<(), String>
where
    F: FnMut(&Path, fs::FileType, Action) -> Result<bool, String>,
{
    let mut entries = fs::read_dir(path)
        .map_err(|err| format!("read {}: {err}", display_path(path)))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| format!("read {}: {err}", display_path(path)))?;

    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let entry_path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|err| format!("stat {}: {err}", display_path(&entry_path)))?;
        let rel = rel_path(root, &entry_path);
        let action = rules.action_for(&rel, file_type.is_dir());
        let descend = visit(&entry_path, file_type, action)?;

        if descend {
            walk_dirs(root, &entry_path, rules, visit)?;
        }
    }

    Ok(())
}

pub fn node_kind(path: &Path, action: Action, metadata: &fs::Metadata) -> String {
    if matches!(action, Action::Secret) {
        "secret"
    } else if metadata.is_dir() && is_repo(path) {
        "repo"
    } else if metadata.is_dir() {
        "directory"
    } else if metadata.file_type().is_symlink() {
        "symlink"
    } else {
        "file"
    }
    .to_string()
}

#[cfg(unix)]
pub fn file_mode(metadata: &fs::Metadata) -> u32 {
    metadata.permissions().mode()
}

#[cfg(not(unix))]
pub fn file_mode(metadata: &fs::Metadata) -> u32 {
    if metadata.permissions().readonly() {
        0o444
    } else {
        0o666
    }
}

pub fn modified_secs(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

pub fn file_hash(path: &Path) -> Result<(String, u64), String> {
    let bytes = fs::read(path).map_err(|err| format!("read {}: {err}", display_path(path)))?;
    let size = bytes.len() as u64;
    let hash = fnv_bytes(&bytes);
    Ok((format!("fnv1a64:{hash:016x}"), size))
}

pub fn object_path(root: &Path, hash: &str) -> PathBuf {
    object_store_path(root).join(hash.replace(':', "_"))
}

pub fn node_id(rel: &str) -> String {
    format!("node_{:016x}", fnv_bytes(rel.as_bytes()))
}

pub fn parent_rel(rel: &str) -> Option<String> {
    if rel == "." {
        None
    } else {
        rel.rsplit_once('/')
            .map(|(parent, _)| parent.to_string())
            .or_else(|| Some(".".to_string()))
    }
}
