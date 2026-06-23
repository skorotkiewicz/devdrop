use crate::commands::{flag_value, optional_path, required_path};
use crate::db::{db_path, init_db, log_operation, operation_sql, query_lines, run_sql};
use crate::fs_util::{
    display_path, find_workspace_root, pin_path, read_pins, rel_path, require_dir,
};
use crate::index::{
    IndexNode, IndexSnapshot, file_hash, file_version_sql, is_conflict_path, mark_node_local,
    node_id, object_path, object_store_path, parent_rel, walk_dirs,
};
use crate::rules::Rules;
use crate::secrets::{secret_cipher_path, secret_store_path, upsert_secret};
use crate::util::{
    hex_decode, hex_decode_string, hex_encode, json_string, now_nanos, now_secs, sql_optional,
    sql_string,
};
use crate::workspace::init_workspace_storage;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

pub fn cmd_remote(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("init") => {
            let path = required_path(args.get(1), "remote init")?;
            init_remote_storage(&path)?;
            println!("remote initialized: {}", display_path(&path));
            Ok(())
        }
        _ => Err("usage: devdrop remote init <path>".into()),
    }
}

pub fn cmd_conflicts(args: &[String]) -> Result<(), String> {
    if args.first().map(String::as_str) == Some("resolve") {
        let path = required_path(args.get(1), "conflicts resolve")?;
        let choice = flag_value(args, "--use").unwrap_or("base");
        return cmd_conflicts_resolve(&path, choice);
    }

    let root = optional_path(args.first())?;
    require_dir(&root)?;
    let rules = Rules::load(&root)?;
    let mut printed = 0;

    walk_dirs(&root, &root, &rules, &mut |path, file_type, action| {
        let rel = rel_path(&root, path);
        if is_conflict_path(&rel) {
            println!("conflicted {}", display_path(path));
            printed += 1;
        }

        Ok(file_type.is_dir() && !action.skips_children())
    })?;

    if printed == 0 {
        println!("no conflicts");
    }

    Ok(())
}

fn cmd_conflicts_resolve(path: &Path, choice: &str) -> Result<(), String> {
    let pair = conflict_pair(path)?;
    let root = find_workspace_root(&pair.base)
        .or_else(|| find_workspace_root(&pair.conflict))
        .ok_or_else(|| "no workspace found; run `devdrop init <path>`".to_string())?;

    match choice {
        "base" | "ours" => {
            archive_conflict_file(&root, &pair.conflict)?;
        }
        "conflict" | "theirs" => {
            if pair.base.exists() {
                archive_conflict_file(&root, &pair.base)?;
            }
            if let Some(parent) = pair.base.parent() {
                fs::create_dir_all(parent)
                    .map_err(|err| format!("create {}: {err}", display_path(parent)))?;
            }
            fs::rename(&pair.conflict, &pair.base)
                .or_else(|_| {
                    fs::copy(&pair.conflict, &pair.base)?;
                    fs::remove_file(&pair.conflict)
                })
                .map_err(|err| format!("resolve conflict: {err}"))?;
        }
        _ => return Err("usage: devdrop conflicts resolve <path> --use base|conflict".into()),
    }

    log_operation(
        &root,
        "conflict_resolve",
        &pin_path(&root, &pair.base),
        &format!("{{\"use\":{}}}", json_string(choice)),
        "done",
    )?;
    println!("resolved conflict: {}", display_path(&pair.base));
    Ok(())
}

struct ConflictPair {
    base: PathBuf,
    conflict: PathBuf,
}

fn conflict_pair(path: &Path) -> Result<ConflictPair, String> {
    if is_conflict_path(&pin_path(Path::new(""), path)) {
        let base = conflict_base_path(path)?;
        if !path.exists() {
            return Err(format!("conflict file not found: {}", display_path(path)));
        }
        return Ok(ConflictPair {
            base,
            conflict: path.to_path_buf(),
        });
    }

    let parent = path
        .parent()
        .ok_or_else(|| format!("no parent directory for {}", display_path(path)))?;
    let matches = fs::read_dir(parent)
        .map_err(|err| format!("read {}: {err}", display_path(parent)))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|candidate| is_conflict_path(&pin_path(Path::new(""), candidate)))
        .filter_map(|candidate| {
            conflict_base_path(&candidate)
                .ok()
                .filter(|base| base == path)
                .map(|_| candidate)
        })
        .collect::<Vec<_>>();

    match matches.as_slice() {
        [conflict] => Ok(ConflictPair {
            base: path.to_path_buf(),
            conflict: conflict.clone(),
        }),
        [] => Err(format!(
            "no conflict sibling found for {}",
            display_path(path)
        )),
        _ => Err(format!(
            "multiple conflict siblings found for {}",
            display_path(path)
        )),
    }
}

pub fn conflict_base_path(path: &Path) -> Result<PathBuf, String> {
    let name = path
        .file_name()
        .ok_or_else(|| format!("no filename for {}", display_path(path)))?
        .to_string_lossy();
    let start = name
        .find(" (conflict from ")
        .ok_or_else(|| format!("not a conflict path: {}", display_path(path)))?;
    let end = name[start..]
        .find(')')
        .map(|end| start + end + 1)
        .ok_or_else(|| format!("bad conflict filename: {}", display_path(path)))?;
    let base_name = format!("{}{}", &name[..start], &name[end..]);
    Ok(path.with_file_name(base_name))
}

fn archive_conflict_file(root: &Path, path: &Path) -> Result<PathBuf, String> {
    if !path.exists() {
        return Err(format!(
            "cannot archive missing file: {}",
            display_path(path)
        ));
    }

    let rel = path.strip_prefix(root).unwrap_or(path);
    let archive = root
        .join(".devdrop/resolved-conflicts")
        .join(now_nanos().to_string())
        .join(rel);
    if let Some(parent) = archive.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("create {}: {err}", display_path(parent)))?;
    }
    fs::rename(path, &archive)
        .or_else(|_| {
            fs::copy(path, &archive)?;
            fs::remove_file(path)
        })
        .map_err(|err| format!("archive {}: {err}", display_path(path)))?;
    println!("archived: {}", display_path(&archive));
    Ok(archive)
}

fn init_remote_storage(remote: &Path) -> Result<(), String> {
    fs::create_dir_all(remote_manifest_dir(remote))
        .map_err(|err| format!("create remote manifest dir: {err}"))?;
    fs::create_dir_all(remote_objects_path(remote))
        .map_err(|err| format!("create remote object dir: {err}"))?;
    fs::create_dir_all(remote_secrets_path(remote))
        .map_err(|err| format!("create remote secret dir: {err}"))?;
    Ok(())
}

fn remote_config_path(root: &Path) -> PathBuf {
    root.join(".devdrop/remote")
}

fn remote_manifest_dir(remote: &Path) -> PathBuf {
    remote.join("manifests")
}

fn remote_manifest_path(remote: &Path) -> PathBuf {
    remote_manifest_dir(remote).join("latest.tsv")
}

fn remote_objects_path(remote: &Path) -> PathBuf {
    remote.join("objects")
}

fn remote_object_path(remote: &Path, hash: &str) -> PathBuf {
    remote_objects_path(remote).join(hash.replace(':', "_"))
}

fn remote_secrets_path(remote: &Path) -> PathBuf {
    remote.join("secrets")
}

fn remote_devices_path(remote: &Path) -> PathBuf {
    remote.join("devices.tsv")
}

fn remote_tombstones_path(remote: &Path) -> PathBuf {
    remote.join("tombstones.tsv")
}

pub fn write_remote_config(root: &Path, remote: &Path) -> Result<(), String> {
    fs::write(
        remote_config_path(root),
        remote.to_string_lossy().as_bytes(),
    )
    .map_err(|err| format!("write remote config: {err}"))
}

pub fn read_remote_config(root: &Path) -> Result<Option<PathBuf>, String> {
    let path = remote_config_path(root);
    if !path.exists() {
        return Ok(None);
    }

    let text = fs::read_to_string(&path).map_err(|err| format!("read remote config: {err}"))?;
    let text = text.trim();
    Ok((!text.is_empty()).then(|| PathBuf::from(text)))
}

fn push_remote_devices(root: &Path, remote: &Path) -> Result<(), String> {
    let manifest_path = remote_devices_path(remote);

    let mut lines_to_write = Vec::new();

    if manifest_path.exists() {
        let text = fs::read_to_string(&manifest_path)
            .map_err(|err| format!("read devices manifest: {err}"))?;
        let mut lines = text.lines();
        if lines.next() == Some("devdrop-devices-v1") {
            for line in lines {
                lines_to_write.push(line.to_string());
            }
        }
    }

    let db = db_path(root);
    if db.exists() {
        for row in query_lines(
            &db,
            "SELECT hex(id)||char(9)||hex(user_id)||char(9)||hex(name)||char(9)||hex(os)||char(9)||hex(arch)||char(9)||hex(trust_level)||char(9)||last_seen_at||char(9)||hex(public_key) FROM devices ORDER BY id;",
        )? {
            if let Some(id_hex) = row.split('\t').next() {
                lines_to_write.retain(|l| l.split('\t').next() != Some(id_hex));
                lines_to_write.push(row);
            }
        }
    }

    let mut out = File::create(&manifest_path)
        .map_err(|err| format!("write remote devices manifest: {err}"))?;
    writeln!(out, "devdrop-devices-v1").map_err(|err| format!("write devices manifest: {err}"))?;
    for line in lines_to_write {
        writeln!(out, "{line}").map_err(|err| format!("write devices manifest: {err}"))?;
    }

    Ok(())
}

fn pull_remote_devices(root: &Path, remote: &Path) -> Result<(), String> {
    let manifest = remote_devices_path(remote);
    if !manifest.exists() {
        return Ok(());
    }

    let text =
        fs::read_to_string(&manifest).map_err(|err| format!("read devices manifest: {err}"))?;
    let mut lines = text.lines();
    if lines.next() != Some("devdrop-devices-v1") {
        return Err("unsupported remote devices manifest".into());
    }

    init_db(root)?;
    let mut sql = String::from("BEGIN;\n");
    for (index, line) in lines.enumerate() {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 8 {
            return Err(format!("bad devices manifest line {}", index + 2));
        }
        let last_seen_at = fields[6]
            .parse::<i64>()
            .map_err(|err| format!("bad device timestamp on line {}: {err}", index + 2))?;
        sql.push_str(&format!(
            "INSERT OR REPLACE INTO devices (id, workspace_id, user_id, name, os, arch, trust_level, last_seen_at, public_key) VALUES ({}, 'local', {}, {}, {}, {}, {}, {}, {});\n",
            sql_string(&hex_decode_string(fields[0])?),
            sql_string(&hex_decode_string(fields[1])?),
            sql_string(&hex_decode_string(fields[2])?),
            sql_string(&hex_decode_string(fields[3])?),
            sql_string(&hex_decode_string(fields[4])?),
            sql_string(&hex_decode_string(fields[5])?),
            last_seen_at,
            sql_string(&hex_decode_string(fields[7])?)
        ));
    }
    sql.push_str("COMMIT;\n");
    run_sql(&db_path(root), &sql)
}

pub fn fetch_remote_object(root: &Path, hash: &str) -> Result<(), String> {
    let remote = read_remote_config(root)?
        .ok_or_else(|| format!("object {hash} is not local and no remote is configured"))?;
    let src = remote_object_path(&remote, hash);
    if !src.exists() {
        return Err(format!("remote object missing: {}", display_path(&src)));
    }
    fs::create_dir_all(object_store_path(root))
        .map_err(|err| format!("create object store: {err}"))?;
    fs::copy(&src, object_path(root, hash))
        .map_err(|err| format!("copy remote object {hash}: {err}"))?;
    Ok(())
}

fn push_remote_secrets(root: &Path, remote: &Path) -> Result<(), String> {
    let manifest_path = remote.join("secrets.tsv");

    let mut lines_to_write = Vec::new();

    if manifest_path.exists() {
        let text = fs::read_to_string(&manifest_path)
            .map_err(|err| format!("read secrets manifest: {err}"))?;
        let mut lines = text.lines();
        if lines.next() == Some("devdrop-secrets-v1") {
            for line in lines {
                lines_to_write.push(line.to_string());
            }
        }
    }

    let db = db_path(root);
    if db.exists() {
        for row in query_lines(
            &db,
            "SELECT hex(path)||char(9)||hex(scope)||char(9)||hex(encrypted_path) FROM secrets ORDER BY path, scope;",
        )? {
            let fields = row.split('\t').collect::<Vec<_>>();
            if fields.len() != 3 {
                continue;
            }
            let key = format!("{}:{}", fields[0], fields[1]);

            lines_to_write.retain(|l| {
                let lf = l.split('\t').collect::<Vec<_>>();
                lf.len() == 3 && format!("{}:{}", lf[0], lf[1]) != key
            });

            let rel = hex_decode_string(fields[0])?;
            let scope = hex_decode_string(fields[1])?;
            let encrypted = PathBuf::from(hex_decode_string(fields[2])?);
            let name = secret_cipher_path(remote, &rel, &scope)
                .file_name()
                .ok_or_else(|| "secret filename".to_string())?
                .to_string_lossy()
                .into_owned();
            fs::copy(&encrypted, remote_secrets_path(remote).join(&name))
                .map_err(|err| format!("copy remote secret {rel}: {err}"))?;

            lines_to_write.push(format!(
                "{}\t{}\t{}",
                hex_encode(rel.as_bytes()),
                hex_encode(scope.as_bytes()),
                hex_encode(name.as_bytes())
            ));
        }
    }

    let mut out = File::create(&manifest_path)
        .map_err(|err| format!("write remote secrets manifest: {err}"))?;
    writeln!(out, "devdrop-secrets-v1").map_err(|err| format!("write secrets manifest: {err}"))?;
    for line in lines_to_write {
        writeln!(out, "{line}").map_err(|err| format!("write secrets manifest: {err}"))?;
    }
    Ok(())
}

fn pull_remote_secrets(root: &Path, remote: &Path) -> Result<(), String> {
    let manifest = remote.join("secrets.tsv");
    if !manifest.exists() {
        return Ok(());
    }

    let text =
        fs::read_to_string(&manifest).map_err(|err| format!("read secrets manifest: {err}"))?;
    let mut lines = text.lines();
    if lines.next() != Some("devdrop-secrets-v1") {
        return Err("unsupported remote secrets manifest".into());
    }

    for (index, line) in lines.enumerate() {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 3 {
            return Err(format!("bad secrets manifest line {}", index + 2));
        }
        let rel = hex_decode_string(fields[0])?;
        let scope = hex_decode_string(fields[1])?;
        let name = hex_decode_string(fields[2])?;
        let local = secret_store_path(root).join(&name);
        fs::copy(remote_secrets_path(remote).join(&name), &local)
            .map_err(|err| format!("copy pulled secret {rel}: {err}"))?;
        upsert_secret(root, &rel, &scope, &local, false)?;
    }
    Ok(())
}

pub fn push_remote(root: &Path, remote: &Path, snapshot: &IndexSnapshot) -> Result<(), String> {
    init_remote_storage(remote)?;

    let existing_remote = read_remote_manifest(remote)?;
    let local_paths = snapshot
        .nodes
        .iter()
        .map(|n| n.path.clone())
        .collect::<HashSet<_>>();

    let local_tombstones = if db_path(root).exists() {
        query_lines(
            &db_path(root),
            "SELECT path FROM tombstones WHERE workspace_id='local';",
        )?
        .into_iter()
        .collect::<HashSet<_>>()
    } else {
        HashSet::new()
    };

    let mut manifest = File::create(remote_manifest_path(remote))
        .map_err(|err| format!("write remote manifest: {err}"))?;
    writeln!(manifest, "devdrop-manifest-v1").map_err(|err| format!("write manifest: {err}"))?;

    for node in snapshot.nodes.iter().filter(|node| remote_node(node)) {
        writeln!(
            manifest,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            hex_encode(node.path.as_bytes()),
            node.kind,
            node.mode,
            node.size,
            node.content_hash.as_deref().unwrap_or(""),
            node.local_state,
            node.local_mtime
        )
        .map_err(|err| format!("write manifest: {err}"))?;
    }

    for node in &existing_remote {
        if local_paths.contains(&node.path) {
            continue;
        }
        if local_tombstones.contains(&node.path) {
            continue;
        }
        writeln!(
            manifest,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            hex_encode(node.path.as_bytes()),
            node.kind,
            node.mode,
            node.size,
            node.content_hash.as_deref().unwrap_or(""),
            node.local_state,
            node.local_mtime
        )
        .map_err(|err| format!("write manifest: {err}"))?;
    }

    for blob in snapshot.blobs.values() {
        let dest = remote_object_path(remote, &blob.hash);
        if !dest.exists() {
            fs::copy(&blob.local_path, &dest)
                .map_err(|err| format!("copy remote blob {}: {err}", blob.hash))?;
        }
    }

    push_remote_secrets(root, remote)?;
    push_remote_devices(root, remote)?;
    push_remote_tombstones(root, remote)?;
    Ok(())
}

pub fn pull_remote(root: &Path, remote: &Path) -> Result<(), String> {
    init_workspace_storage(root)?;
    let nodes = read_remote_manifest(remote)?;
    let tombstones = read_remote_tombstones(remote)?;

    for node in &nodes {
        if matches!(node.kind.as_str(), "directory" | "repo") && node.path != "." {
            fs::create_dir_all(root.join(&node.path))
                .map_err(|err| format!("create {}: {err}", node.path))?;
        }
    }

    pull_remote_secrets(root, remote)?;
    pull_remote_devices(root, remote)?;
    materialize_pull_conflicts(root, remote, &nodes)?;
    apply_remote_tombstones(root, &tombstones)?;
    write_pulled_index(root, &nodes)?;
    hydrate_pins_from_remote(root, remote, &nodes)?;
    Ok(())
}

fn materialize_pull_conflicts(
    root: &Path,
    remote: &Path,
    nodes: &[RemoteNode],
) -> Result<(), String> {
    for node in nodes.iter().filter(|node| node.kind == "file") {
        let Some(hash) = &node.content_hash else {
            continue;
        };
        let path = root.join(&node.path);
        if !path.is_file() {
            continue;
        }

        let (local_hash, _) = file_hash(&path)?;
        if &local_hash == hash {
            continue;
        }

        let object = object_path(root, hash);
        if !object.exists() {
            let remote_object = remote_object_path(remote, hash);
            fs::copy(&remote_object, &object).map_err(|err| {
                format!("copy remote object {}: {err}", display_path(&remote_object))
            })?;
        }

        let conflict = conflict_sibling_path(&path, "remote")?;
        if let Some(parent) = conflict.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("create {}: {err}", display_path(parent)))?;
        }
        fs::copy(&object, &conflict)
            .map_err(|err| format!("write conflict {}: {err}", display_path(&conflict)))?;
        log_operation(
            root,
            "pull_conflict",
            &node.path,
            &format!("{{\"remote_hash\":{}}}", json_string(hash)),
            "conflicted",
        )?;
        println!(
            "conflict: kept local {}, wrote remote {}",
            display_path(&path),
            display_path(&conflict)
        );
    }

    Ok(())
}

fn conflict_sibling_path(path: &Path, source: &str) -> Result<PathBuf, String> {
    let name = path
        .file_name()
        .ok_or_else(|| format!("no filename for {}", display_path(path)))?
        .to_string_lossy();
    let split = name
        .rfind('.')
        .filter(|index| *index > 0)
        .unwrap_or(name.len());
    let marker = format!(" (conflict from {source} {})", now_nanos());
    Ok(path.with_file_name(format!("{}{}{}", &name[..split], marker, &name[split..])))
}

fn hydrate_pins_from_remote(
    root: &Path,
    remote: &Path,
    nodes: &[RemoteNode],
) -> Result<(), String> {
    let pins = read_pins(root)?;
    if pins.is_empty() {
        return Ok(());
    }

    for node in nodes.iter().filter(|node| node.kind == "file") {
        let Some(hash) = &node.content_hash else {
            continue;
        };
        if !pins.iter().any(|pin| pin_matches(pin, &node.path)) {
            continue;
        }

        let path = root.join(&node.path);
        if path.exists() {
            continue;
        }

        let object = object_path(root, hash);
        if !object.exists() {
            let remote_object = remote_object_path(remote, hash);
            fs::copy(&remote_object, &object).map_err(|err| {
                format!("copy remote object {}: {err}", display_path(&remote_object))
            })?;
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("create {}: {err}", display_path(parent)))?;
        }
        fs::copy(&object, &path)
            .map_err(|err| format!("hydrate pinned {}: {err}", display_path(&path)))?;
        mark_node_local(root, &node.path)?;
        log_operation(root, "hydrate_pinned", &node.path, "{}", "done")?;
        println!("hydrated pinned: {}", display_path(&path));
    }

    Ok(())
}

fn pin_matches(pin: &str, rel: &str) -> bool {
    pin == "."
        || rel == pin
        || rel
            .strip_prefix(pin)
            .is_some_and(|rest| rest.starts_with('/'))
}

struct RemoteTombstone {
    path: String,
    content_hash: Option<String>,
    deleted_at: i64,
}

fn push_remote_tombstones(root: &Path, remote: &Path) -> Result<(), String> {
    let manifest_path = remote_tombstones_path(remote);

    let mut lines_to_write = Vec::new();
    let mut existing_keys = HashSet::new();

    if manifest_path.exists() {
        let text =
            fs::read_to_string(&manifest_path).map_err(|err| format!("read tombstones: {err}"))?;
        let mut lines = text.lines();
        if lines.next() == Some("devdrop-tombstones-v1") {
            for line in lines {
                let fields = line.split('\t').collect::<Vec<_>>();
                if fields.len() == 3 {
                    let key = format!("{}:{}", fields[0], fields[1]);
                    if existing_keys.insert(key) {
                        lines_to_write.push(line.to_string());
                    }
                }
            }
        }
    }

    let db = db_path(root);
    if db.exists() {
        for row in query_lines(
            &db,
            "SELECT hex(path)||char(9)||ifnull(hex(content_hash),'')||char(9)||deleted_at FROM tombstones ORDER BY deleted_at, path;",
        )? {
            let fields = row.split('\t').collect::<Vec<_>>();
            if fields.len() != 3 {
                continue;
            }
            let key = format!("{}:{}", fields[0], fields[1]);
            if existing_keys.insert(key) {
                lines_to_write.push(row);
            }
        }
    }

    let mut out =
        File::create(&manifest_path).map_err(|err| format!("write remote tombstones: {err}"))?;
    writeln!(out, "devdrop-tombstones-v1").map_err(|err| format!("write tombstones: {err}"))?;
    for line in lines_to_write {
        writeln!(out, "{line}").map_err(|err| format!("write tombstones: {err}"))?;
    }

    Ok(())
}

fn read_remote_tombstones(remote: &Path) -> Result<Vec<RemoteTombstone>, String> {
    let path = remote_tombstones_path(remote);
    if !path.exists() {
        return Ok(Vec::new());
    }

    let text = fs::read_to_string(&path).map_err(|err| format!("read tombstones: {err}"))?;
    let mut lines = text.lines();
    if lines.next() != Some("devdrop-tombstones-v1") {
        return Err("unsupported remote tombstones".into());
    }

    lines
        .enumerate()
        .map(|(index, line)| {
            let fields = line.split('\t').collect::<Vec<_>>();
            if fields.len() != 3 {
                return Err(format!("bad tombstone line {}", index + 2));
            }
            Ok(RemoteTombstone {
                path: hex_decode_string(fields[0])?,
                content_hash: (!fields[1].is_empty())
                    .then(|| hex_decode_string(fields[1]))
                    .transpose()?,
                deleted_at: fields[2].parse().map_err(|err| {
                    format!("bad tombstone timestamp on line {}: {err}", index + 2)
                })?,
            })
        })
        .collect()
}

fn apply_remote_tombstones(root: &Path, tombstones: &[RemoteTombstone]) -> Result<(), String> {
    for tombstone in tombstones {
        let path = root.join(&tombstone.path);
        if !path.is_file() {
            continue;
        }

        let (local_hash, _) = file_hash(&path)?;
        if tombstone.content_hash.as_deref() == Some(local_hash.as_str()) {
            fs::remove_file(&path)
                .map_err(|err| format!("delete {}: {err}", display_path(&path)))?;
            continue;
        }

        let conflict = conflict_sibling_path(&path, "local")?;
        if let Some(parent) = conflict.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("create {}: {err}", display_path(parent)))?;
        }
        fs::rename(&path, &conflict)
            .or_else(|_| {
                fs::copy(&path, &conflict)?;
                fs::remove_file(&path)
            })
            .map_err(|err| format!("write delete conflict {}: {err}", display_path(&conflict)))?;
        log_operation(
            root,
            "delete_conflict",
            &tombstone.path,
            &format!("{{\"deleted_at\":{}}}", tombstone.deleted_at),
            "conflicted",
        )?;
        println!(
            "conflict: remote deleted {}, kept local {}",
            display_path(&path),
            display_path(&conflict)
        );
    }

    Ok(())
}

fn remote_node(node: &IndexNode) -> bool {
    !matches!(node.local_state.as_str(), "local-only" | "ignored")
        && node.path != ".devdrop"
        && !node.path.starts_with(".devdrop/")
}

struct RemoteNode {
    path: String,
    kind: String,
    mode: u32,
    size: u64,
    content_hash: Option<String>,
    local_state: String,
    local_mtime: i64,
}

fn read_remote_manifest(remote: &Path) -> Result<Vec<RemoteNode>, String> {
    let path = remote_manifest_path(remote);
    if !path.exists() {
        return Ok(Vec::new());
    }

    let text = fs::read_to_string(&path).map_err(|err| format!("read remote manifest: {err}"))?;
    let mut lines = text.lines();
    if lines.next() != Some("devdrop-manifest-v1") {
        return Err("unsupported remote manifest".into());
    }

    lines
        .enumerate()
        .map(|(index, line)| parse_remote_node(line, index + 2))
        .collect()
}

fn parse_remote_node(line: &str, line_no: usize) -> Result<RemoteNode, String> {
    let fields = line.split('\t').collect::<Vec<_>>();
    if fields.len() != 7 {
        return Err(format!("bad manifest line {line_no}"));
    }

    Ok(RemoteNode {
        path: String::from_utf8(hex_decode(fields[0])?)
            .map_err(|err| format!("bad manifest path on line {line_no}: {err}"))?,
        kind: fields[1].to_string(),
        mode: fields[2]
            .parse()
            .map_err(|err| format!("bad mode on line {line_no}: {err}"))?,
        size: fields[3]
            .parse()
            .map_err(|err| format!("bad size on line {line_no}: {err}"))?,
        content_hash: (!fields[4].is_empty()).then(|| fields[4].to_string()),
        local_state: fields[5].to_string(),
        local_mtime: fields[6]
            .parse()
            .map_err(|err| format!("bad mtime on line {line_no}: {err}"))?,
    })
}

fn write_pulled_index(root: &Path, nodes: &[RemoteNode]) -> Result<(), String> {
    init_db(root)?;
    let db = db_path(root);
    let now = now_secs();
    let mut sql = String::from(
        "
BEGIN;
DELETE FROM nodes;
DELETE FROM blobs;
DELETE FROM repo_status;
",
    );

    for node in nodes {
        let local_state = pulled_state(node);
        sql.push_str(&format!(
            "INSERT OR REPLACE INTO nodes (id, workspace_id, parent_id, path, kind, mode, size, content_hash, local_state, remote_manifest_id, local_mtime, remote_mtime, deleted_at) VALUES ({}, 'local', {}, {}, {}, {}, {}, {}, {}, 'latest', NULL, {}, NULL);\n",
            sql_string(&node_id(&node.path)),
            sql_optional(parent_rel(&node.path).as_deref()),
            sql_string(&node.path),
            sql_string(&node.kind),
            node.mode,
            node.size,
            sql_optional(node.content_hash.as_deref()),
            sql_string(local_state),
            node.local_mtime
        ));

        if let Some(hash) = &node.content_hash {
            sql.push_str(&format!(
                "INSERT OR REPLACE INTO blobs (hash, size, local_path, present, ref_count) VALUES ({}, {}, {}, {}, 1);\n",
                sql_string(hash),
                node.size,
                sql_string(&object_path(root, hash).to_string_lossy()),
                i32::from(object_path(root, hash).exists())
            ));
            sql.push_str(&file_version_sql(
                &node.path,
                hash,
                node.size,
                &object_path(root, hash),
                now,
            ));
        }
    }

    sql.push_str(&operation_sql(
        "pull",
        ".",
        &format!("{{\"nodes\":{}}}", nodes.len()),
        "done",
        now,
    ));
    sql.push_str("COMMIT;\n");
    run_sql(&db, &sql)
}

fn pulled_state(node: &RemoteNode) -> &str {
    match node.kind.as_str() {
        "directory" | "repo" => "local",
        "secret" => "secret-locked",
        _ if node.local_state == "metadata-only" => "metadata-only",
        _ if node.content_hash.is_some() => "remote-only",
        _ => node.local_state.as_str(),
    }
}
