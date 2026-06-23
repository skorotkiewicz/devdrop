use super::agent::{agent_base_path, agent_overlay_path, update_agent_status, upsert_agent};
use super::commands::{first_positional, flag_value, optional_path, required_arg, required_path};
use super::crypto::command_ok;
use super::db::{
    current_user, db_path, device_count, init_db, log_operation, query_lines, run_sql, upsert_user,
};
use super::fs_util::{
    copy_tree, display_path, find_workspace_root, pin_path, read_pins, rel_path, require_dir,
    sync_tree, write_pins,
};
use super::git::is_repo;
use super::index::{
    Counts, file_versions, hydrate_from_local_store, indexed_entries_in_dir, indexed_state_count,
    latest_file_version_hash, local_state_for, object_path, object_store_path, scan_workspace,
    walk_dirs,
};
use super::remote::{fetch_remote_object, read_remote_config, write_remote_url};
use super::rules::{Action, Rules};
use super::secrets::{locked_secret_count, locked_secrets_in_dir, secret_store_path};
use super::util::{
    current_arch, current_os, fnv_bytes, json_string, now_nanos, now_secs, sql_string,
};
use std::collections::HashSet;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub fn cmd_init(args: &[String]) -> Result<(), String> {
    let root = optional_path(first_positional(args))?;
    let user = default_user();
    let remote = flag_value(args, "--remote").map(str::to_string);

    init_workspace_storage(&root)?;
    upsert_user(&root, &user)?;

    let device = if device_count(&root)? == 0 {
        let name = default_device_name();
        Some((upsert_device(&root, &user, &name)?, name))
    } else {
        None
    };

    if let Some(remote) = &remote {
        write_remote_url(&root, remote)?;
    }

    log_operation(
        &root,
        "init",
        ".",
        &format!("{{\"user\":{}}}", json_string(&user)),
        "done",
    )?;
    println!("workspace: {}", display_path(&root));
    println!("user: {user}");
    if let Some((id, name)) = device {
        println!("device: {name} ({id})");
    }
    println!(
        "remote: {}",
        remote
            .as_ref()
            .cloned()
            .unwrap_or_else(|| "not configured".to_string())
    );
    println!("next: devdrop sync {}", display_path(&root));
    Ok(())
}

pub fn cmd_login(user: Option<&String>) -> Result<(), String> {
    let cwd = env::current_dir().map_err(|err| format!("current dir: {err}"))?;
    let existing = find_workspace_root(&cwd);
    if existing.is_none() {
        println!("no workspace found; initializing at {}", display_path(&cwd));
    }
    let root = existing.unwrap_or(cwd);

    init_workspace_storage(&root)?;
    let user = user.cloned().unwrap_or_else(default_user);
    init_db(&root)?;
    upsert_user(&root, &user)?;
    log_operation(
        &root,
        "login",
        ".",
        &format!("{{\"user\":{}}}", json_string(&user)),
        "done",
    )?;
    println!("logged in: {user}");
    Ok(())
}

pub fn cmd_workspace(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("init") | Some("mount") => {
            let path = required_path(args.get(1), "workspace init|mount")?;
            init_workspace_storage(&path)?;
            println!("workspace initialized: {}", display_path(&path));
            Ok(())
        }
        _ => Err("usage: devdrop workspace init|mount <path>".into()),
    }
}

pub fn cmd_device(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("enroll") => {
            let name = required_arg(args.get(1), "device enroll")?;
            cmd_device_enroll(name)
        }
        Some("list") => cmd_device_list(),
        _ => Err("usage: devdrop device enroll <name> | devdrop device list".into()),
    }
}

pub fn cmd_device_enroll(name: &str) -> Result<(), String> {
    let root = env::current_dir()
        .map_err(|err| format!("current dir: {err}"))
        .ok()
        .and_then(|dir| find_workspace_root(&dir))
        .ok_or_else(|| "no workspace found; run `devdrop init .`".to_string())?;
    init_db(&root)?;
    let user = current_user(&root)?.unwrap_or_else(|| "local".to_string());
    upsert_user(&root, &user)?;
    let id = upsert_device(&root, &user, name)?;
    log_operation(
        &root,
        "device_enroll",
        ".",
        &format!("{{\"device\":{}}}", json_string(&id)),
        "done",
    )?;
    println!("device enrolled: {id} {name}");
    Ok(())
}

pub fn cmd_device_list() -> Result<(), String> {
    let root = env::current_dir()
        .map_err(|err| format!("current dir: {err}"))
        .ok()
        .and_then(|dir| find_workspace_root(&dir))
        .ok_or_else(|| "no workspace found; run `devdrop init .`".to_string())?;
    init_db(&root)?;
    let rows = query_lines(
        &db_path(&root),
        "SELECT id||char(9)||user_id||char(9)||name||char(9)||os||char(9)||arch||char(9)||trust_level||char(9)||last_seen_at FROM devices ORDER BY last_seen_at DESC, name;",
    )?;
    if rows.is_empty() {
        println!("no devices enrolled");
        return Ok(());
    }

    for row in rows {
        let fields = row.split('\t').collect::<Vec<_>>();
        if fields.len() == 7 {
            println!(
                "{} user={} name={} os={} arch={} trust={} last_seen={}",
                fields[0], fields[1], fields[2], fields[3], fields[4], fields[5], fields[6]
            );
        }
    }
    Ok(())
}

pub fn default_user() -> String {
    env::var("USER")
        .or_else(|_| env::var("USERNAME"))
        .unwrap_or_else(|_| "local".to_string())
}

pub fn default_device_name() -> String {
    env::var("HOSTNAME")
        .or_else(|_| env::var("COMPUTERNAME"))
        .ok()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "local-device".to_string())
}

pub fn upsert_device(root: &Path, user: &str, name: &str) -> Result<String, String> {
    let id = format!(
        "device_{:016x}",
        fnv_bytes(format!("{user}:{name}").as_bytes())
    );
    let public_key = format!(
        "local-pub-{:016x}",
        fnv_bytes(format!("{id}:{}", now_nanos()).as_bytes())
    );
    let now = now_secs();
    run_sql(
        &db_path(root),
        &format!(
            "INSERT OR REPLACE INTO devices (id, workspace_id, user_id, name, os, arch, trust_level, last_seen_at, public_key) VALUES ({}, 'local', {}, {}, {}, {}, 'personal', {}, {});\n",
            sql_string(&id),
            sql_string(user),
            sql_string(name),
            sql_string(current_os()),
            sql_string(current_arch()),
            now,
            sql_string(&public_key)
        ),
    )?;
    Ok(id)
}

pub fn cmd_status(args: &[String]) -> Result<(), String> {
    let root = optional_path(first_positional(args))?;
    let json = args.iter().any(|arg| arg == "--json");
    require_dir(&root)?;

    let rules = Rules::load(&root)?;
    let mut counts = scan_workspace(&root, &rules)?;
    counts.secret_locked += locked_secret_count(&root).unwrap_or(0);
    counts.remote_only += indexed_state_count(&root, "remote-only").unwrap_or(0);
    let pins = read_pins(&root).unwrap_or_default();
    let index = if db_path(&root).exists() {
        "present"
    } else {
        "missing"
    };
    let remote = read_remote_config(&root).ok().flatten();

    if json {
        println!(
            "{{\"workspace\":{},\"index\":{},\"entries\":{},\"local\":{},\"ignored\":{},\"metadata_only\":{},\"local_only\":{},\"remote_only\":{},\"secret_locked\":{},\"conflicted\":{},\"repos\":{},\"dirty_repos\":{},\"pins\":{}}}",
            json_string(&display_path(&root)),
            json_string(index),
            counts.entries,
            counts.local,
            counts.ignored,
            counts.metadata_only,
            counts.local_only,
            counts.remote_only,
            counts.secret_locked,
            counts.conflicted,
            counts.repos,
            counts.dirty_repos,
            pins.len()
        );
        return Ok(());
    }

    println!("workspace: {}", display_path(&root));
    println!("index: {index}");
    println!("entries: {}", counts.entries);
    println!("local: {}", counts.local);
    println!("ignored: {}", counts.ignored);
    println!("metadata-only: {}", counts.metadata_only);
    println!("local-only: {}", counts.local_only);
    println!("remote-only: {}", counts.remote_only);
    println!("secret-locked: {}", counts.secret_locked);
    println!("conflicted: {}", counts.conflicted);
    println!("repos: {}", counts.repos);
    println!("dirty repos: {}", counts.dirty_repos);
    println!("pins: {}", pins.len());
    println!(
        "remote: {}",
        remote
            .as_ref()
            .cloned()
            .unwrap_or_else(|| "not configured".to_string())
    );
    println!(
        "next: {}",
        status_next(&root, &counts, index, remote.as_deref())
    );
    Ok(())
}

pub fn status_next(root: &Path, counts: &Counts, index: &str, remote: Option<&str>) -> String {
    let root = display_path(root);
    if index == "missing" {
        return format!("devdrop init {root}");
    }
    if counts.conflicted > 0 {
        return format!("devdrop conflicts {root}");
    }
    if counts.dirty_repos > 0 {
        return format!("devdrop repo-status {root}");
    }
    if remote.is_some() {
        return format!("devdrop sync {root}");
    }
    format!("devdrop init {root} --remote <path>")
}

pub fn cmd_ls(root: PathBuf) -> Result<(), String> {
    require_dir(&root)?;
    let rules = Rules::load(&root)?;
    let mut entries = fs::read_dir(&root)
        .map_err(|err| format!("read {}: {err}", display_path(&root)))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| format!("read {}: {err}", display_path(&root)))?;
    let mut visible_names = HashSet::new();

    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        visible_names.insert(entry.file_name().to_string_lossy().into_owned());
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|err| format!("stat {}: {err}", display_path(&path)))?;
        let rel = rel_path(&root, &path);
        let action = rules.action_for(&rel, file_type.is_dir());
        let state = local_state_for(&rel, action);
        let slash = if file_type.is_dir() { "/" } else { "" };
        let repo = if file_type.is_dir() && is_repo(&path) {
            " repo"
        } else {
            ""
        };

        println!(
            "{:<13} {}{}{}",
            state,
            entry.file_name().to_string_lossy(),
            slash,
            repo
        );
    }

    for name in locked_secrets_in_dir(&root)? {
        if visible_names.insert(name.clone()) {
            println!("{:<13} {}", "secret-locked", name);
        }
    }

    for entry in indexed_entries_in_dir(&root)? {
        if visible_names.insert(entry.name.clone()) {
            let slash = if entry.kind == "directory" || entry.kind == "repo" {
                "/"
            } else {
                ""
            };
            println!("{:<13} {}{}", entry.state, entry.name, slash);
        }
    }

    Ok(())
}

pub fn cmd_ignored(root: PathBuf) -> Result<(), String> {
    require_dir(&root)?;
    let rules = Rules::load(&root)?;
    let mut printed = 0;

    walk_dirs(&root, &root, &rules, &mut |path, file_type, action| {
        if action != Action::Sync {
            println!("{} {}", action.state_label(), display_path(path));
            printed += 1;
        }

        Ok(file_type.is_dir() && !action.skips_children())
    })?;

    if printed == 0 {
        println!("no ignored paths");
    }

    Ok(())
}

pub fn cmd_doctor(root: PathBuf) -> Result<(), String> {
    require_dir(&root)?;
    let rules = Rules::load(&root)?;
    let db = db_path(&root);
    let objects = object_store_path(&root);
    let secrets = secret_store_path(&root);
    let remote = read_remote_config(&root).ok().flatten();
    let user = current_user(&root).ok().flatten();
    let devices = device_count(&root).unwrap_or(0);

    println!("workspace: ok {}", display_path(&root));
    println!(
        "rules: ok {} default, {} .devsyncignore",
        rules.default_count, rules.custom_count
    );
    println!(
        "git: {}",
        if command_ok("git", &["--version"]) {
            "ok"
        } else {
            "missing"
        }
    );
    println!(
        "sqlite3: {}",
        if command_ok("sqlite3", &["--version"]) {
            "ok"
        } else {
            "missing"
        }
    );
    println!(
        "metadata db: {}",
        if db.exists() { "ok" } else { "missing" }
    );
    println!(
        "auth: {}",
        user.as_deref()
            .map(|user| format!("ok user={user}"))
            .unwrap_or_else(|| "not logged in".to_string())
    );
    println!("devices: {devices}");
    println!(
        "object store: {}",
        if objects.is_dir() { "ok" } else { "missing" }
    );
    println!("daemon: ok polling");
    println!("watcher: ok polling");
    println!(
        "remote manifest: {}",
        remote
            .as_ref()
            .map(|url| format!("configured {url}"))
            .unwrap_or_else(|| "not configured".to_string())
    );
    println!(
        "secret vault: {}",
        if secrets.is_dir() {
            "ok local encrypted"
        } else {
            "missing"
        }
    );
    Ok(())
}

pub fn cmd_hydrate(path: PathBuf) -> Result<(), String> {
    if path.exists() {
        println!("already local: {}", display_path(&path));
        Ok(())
    } else {
        hydrate_from_local_store(&path)
    }
}

pub fn cmd_history(path: PathBuf) -> Result<(), String> {
    let root = find_workspace_root(&path)
        .or_else(|| path.parent().and_then(find_workspace_root))
        .ok_or_else(|| "no workspace found; run `devdrop init <path>`".to_string())?;
    let rel = pin_path(&root, &path);
    let rows = file_versions(&root, &rel)?;
    if rows.is_empty() {
        println!("no history for {rel}");
        return Ok(());
    }

    for row in rows {
        println!(
            "{} size={} seen_at={} present={}",
            row.content_hash,
            row.size,
            row.seen_at,
            object_path(&root, &row.content_hash).exists()
        );
    }
    Ok(())
}

pub fn cmd_recover(args: &[String]) -> Result<(), String> {
    let path = required_path(args.first(), "recover")?;
    let root = find_workspace_root(&path)
        .or_else(|| path.parent().and_then(find_workspace_root))
        .ok_or_else(|| "no workspace found; run `devdrop init <path>`".to_string())?;
    let rel = pin_path(&root, &path);
    let hash = flag_value(args, "--hash")
        .map(str::to_string)
        .or_else(|| latest_file_version_hash(&root, &rel).ok().flatten())
        .ok_or_else(|| format!("no history for {rel}"))?;
    let object = object_path(&root, &hash);
    if !object.exists() {
        fetch_remote_object(&root, &hash)?;
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("create {}: {err}", display_path(parent)))?;
    }
    fs::copy(&object, &path).map_err(|err| format!("recover {}: {err}", display_path(&path)))?;
    log_operation(
        &root,
        "recover",
        &rel,
        &format!("{{\"hash\":{}}}", json_string(&hash)),
        "done",
    )?;
    println!("recovered: {rel} {hash}");
    Ok(())
}

pub fn cmd_pin(path: PathBuf, pin: bool) -> Result<(), String> {
    let root = find_workspace_root(&path)
        .ok_or_else(|| "no workspace found; run `devdrop init <path>`".to_string())?;
    let pins_path = root.join(".devdrop/pins");
    let mut pins = read_pins(&root)?;
    let rel = pin_path(&root, &path);

    if pin {
        if !pins.iter().any(|pin| pin == &rel) {
            pins.push(rel.clone());
            write_pins(&pins_path, &pins)?;
        }
        log_operation(&root, "pin", &rel, "{}", "done")?;
        println!("pinned: {rel}");
    } else {
        pins.retain(|pin| pin != &rel);
        write_pins(&pins_path, &pins)?;
        log_operation(&root, "unpin", &rel, "{}", "done")?;
        println!("unpinned: {rel}");
    }

    Ok(())
}

pub fn cmd_edit(path: PathBuf) -> Result<(), String> {
    let root = find_workspace_root(&path)
        .ok_or_else(|| "no workspace found; run `devdrop init <path>`".to_string())?;
    let repo = if path.is_dir() {
        path
    } else {
        path.parent().unwrap().to_path_buf()
    };

    // Create agent overlay
    let id = format!("edit_{}", now_nanos());
    let overlay = agent_overlay_path(&root, &id);
    let base = agent_base_path(&root, &id);
    copy_tree(&repo, &base)?;
    copy_tree(&base, &overlay)?;
    upsert_agent(&root, &id, &repo, &overlay, "**", "", "pending")?;

    println!("Editing overlay: {}", display_path(&overlay));
    println!("Base: {}", display_path(&repo));

    // Open in $EDITOR
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    let status = std::process::Command::new(&editor)
        .arg(&overlay)
        .status()
        .map_err(|err| format!("launch editor {}: {err}", editor))?;

    if !status.success() {
        return Err(format!("editor exited with {status}"));
    }

    // Show diff
    let output = std::process::Command::new("diff")
        .args(["-ruN", "--exclude=.git", "--exclude=.devdrop"])
        .arg(&repo)
        .arg(&overlay)
        .output()
        .map_err(|err| format!("run diff: {err}"))?;
    print!("{}", String::from_utf8_lossy(&output.stdout));
    eprint!("{}", String::from_utf8_lossy(&output.stderr));

    // Prompt to accept
    print!("Accept changes? [y/N] ");
    std::io::stdout().flush().ok();
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).ok();
    let accept = input.trim().eq_ignore_ascii_case("y");

    if accept {
        sync_tree(std::path::Path::new(&overlay), std::path::Path::new(&repo))?;
        update_agent_status(&root, &id, "accepted")?;
        println!("Changes accepted");
    } else {
        update_agent_status(&root, &id, "rejected")?;
        println!("Changes rejected");
    }
    Ok(())
}

pub fn init_workspace_storage(root: &Path) -> Result<(), String> {
    fs::create_dir_all(root).map_err(|err| format!("create workspace: {err}"))?;
    fs::create_dir_all(root.join(".devdrop")).map_err(|err| format!("create .devdrop: {err}"))?;
    fs::create_dir_all(object_store_path(root))
        .map_err(|err| format!("create object store: {err}"))?;
    fs::create_dir_all(secret_store_path(root))
        .map_err(|err| format!("create secret vault: {err}"))?;
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join(".devdrop/pins"))
        .map_err(|err| format!("create pins file: {err}"))?;
    init_db(root)?;
    Ok(())
}
