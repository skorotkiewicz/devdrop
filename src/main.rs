use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<(), String> {
    let args = env::args().skip(1).collect::<Vec<_>>();

    match args.first().map(String::as_str) {
        None | Some("help") | Some("-h") | Some("--help") => {
            print_help();
            Ok(())
        }
        Some("workspace") => cmd_workspace(&args[1..]),
        Some("repo") => cmd_repo(&args[1..]),
        Some("remote") => cmd_remote(&args[1..]),
        Some("secret") => cmd_secret(&args[1..]),
        Some("run") => cmd_run(&args[1..]),
        Some("sync") => cmd_sync(&args[1..]),
        Some("status") => cmd_status(optional_path(args.get(1))?),
        Some("ls") => cmd_ls(optional_path(args.get(1))?),
        Some("ignored") => cmd_ignored(optional_path(args.get(1))?),
        Some("conflicts") => cmd_conflicts(optional_path(args.get(1))?),
        Some("repo-status") => cmd_repo_status(optional_path(args.get(1))?),
        Some("doctor") => cmd_doctor(optional_path(args.get(1))?),
        Some("hydrate") => cmd_hydrate(required_path(args.get(1), "hydrate")?),
        Some("pin") => cmd_pin(required_path(args.get(1), "pin")?, true),
        Some("unpin") => cmd_pin(required_path(args.get(1), "unpin")?, false),
        Some(other) => Err(format!("unknown command `{other}`; run `devdrop help`")),
    }
}

fn print_help() {
    println!(
        "\
devdrop - local-first workspace helper

Usage:
  devdrop workspace init <path>
  devdrop repo update [path]
  devdrop remote init <path>
  devdrop secret add <path> --scope <scope>
  devdrop secret unlock <path> [--scope <scope>]
  devdrop secret lock <path> [--scope <scope>]
  devdrop run --repo <path> --secret-scope <scope> -- <command>
  devdrop sync [path] [--remote <path>] [--pull]
  devdrop status [path]
  devdrop ls [path]
  devdrop ignored [path]
  devdrop conflicts [path]
  devdrop repo-status [path]
  devdrop hydrate <path>
  devdrop pin <path>
  devdrop unpin <path>
  devdrop doctor [path]"
    );
}

fn cmd_workspace(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("init") | Some("mount") => {
            let path = required_path(args.get(1), "workspace init")?;
            init_workspace_storage(&path)?;
            println!("workspace initialized: {}", display_path(&path));
            Ok(())
        }
        _ => Err("usage: devdrop workspace init <path>".into()),
    }
}

fn cmd_repo(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("update") => {
            let path = optional_path(args.get(1))?;
            require_dir(&path)?;
            if !is_repo(&path) {
                return Err(format!("not a git repo: {}", display_path(&path)));
            }

            run_git(&path, &["fetch", "--prune"])?;
            run_git(&path, &["merge", "--ff-only", "@{u}"])?;
            println!("repo updated: {}", display_path(&path));
            Ok(())
        }
        _ => Err("usage: devdrop repo update [path]".into()),
    }
}

fn cmd_remote(args: &[String]) -> Result<(), String> {
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

fn cmd_secret(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("add") => {
            let path = required_path(args.get(1), "secret add")?;
            let scope = flag_value(args, "--scope").unwrap_or("dev");
            cmd_secret_add(&path, scope)
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
        _ => Err("usage: devdrop secret add|unlock|lock <path> [--scope <scope>]".into()),
    }
}

fn cmd_run(args: &[String]) -> Result<(), String> {
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
    let root = find_workspace_root(path).ok_or_else(|| {
        "no .devdrop workspace found; run `devdrop workspace init <path>`".to_string()
    })?;
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
    let root = find_workspace_root(path).ok_or_else(|| {
        "no .devdrop workspace found; run `devdrop workspace init <path>`".to_string()
    })?;
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

fn cmd_secret_lock(path: &Path, scope: &str) -> Result<(), String> {
    let root = find_workspace_root(path).ok_or_else(|| {
        "no .devdrop workspace found; run `devdrop workspace init <path>`".to_string()
    })?;
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

fn cmd_sync(args: &[String]) -> Result<(), String> {
    let root = optional_path(first_positional(args))?;
    let root = workspace_root_for(&root)?;
    let remote = flag_value(args, "--remote")
        .map(PathBuf::from)
        .or_else(|| read_remote_config(&root).ok().flatten());

    if args.iter().any(|arg| arg == "--pull") {
        let remote = remote.ok_or_else(|| "sync --pull requires --remote <path>".to_string())?;
        pull_remote(&root, &remote)?;
        write_remote_config(&root, &remote)?;
        println!("pulled remote manifest: {}", display_path(&remote));
        return Ok(());
    }

    let snapshot = sync_local_index(&root)?;
    if let Some(remote) = remote {
        push_remote(&root, &remote, &snapshot)?;
        write_remote_config(&root, &remote)?;
        println!("pushed remote manifest: {}", display_path(&remote));
    }

    println!("synced local index: {}", display_path(&root));
    println!("nodes: {}", snapshot.nodes.len());
    println!("blobs: {}", snapshot.blobs.len());
    println!("repos: {}", snapshot.repos.len());
    Ok(())
}

fn sync_local_index(root: &Path) -> Result<IndexSnapshot, String> {
    let rules = Rules::load(root)?;
    let snapshot = collect_index(root, &rules)?;
    write_index(root, &rules, &snapshot)?;
    Ok(snapshot)
}

fn cmd_status(root: PathBuf) -> Result<(), String> {
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
    Ok(())
}

fn cmd_ls(root: PathBuf) -> Result<(), String> {
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

fn cmd_ignored(root: PathBuf) -> Result<(), String> {
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

fn cmd_conflicts(root: PathBuf) -> Result<(), String> {
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

fn cmd_repo_status(root: PathBuf) -> Result<(), String> {
    require_dir(&root)?;
    let rules = Rules::load(&root)?;
    let repos = find_repos(&root, &rules)?;

    if repos.is_empty() {
        println!("no git repos found");
        return Ok(());
    }

    for repo in repos {
        let status = repo_status(&repo);
        let upstream = status.upstream.clone().unwrap_or_else(|| "none".into());
        println!(
            "{} branch={} head={} dirty={} ahead={} behind={} upstream={}",
            display_path(&repo),
            status.branch.as_deref().unwrap_or("?"),
            status.head.as_deref().unwrap_or("?"),
            status.dirty,
            status
                .ahead
                .map(|count| count.to_string())
                .unwrap_or_else(|| "?".into()),
            status
                .behind
                .map(|count| count.to_string())
                .unwrap_or_else(|| "?".into()),
            upstream
        );
        if let Some(warning) = stale_repo_warning(&repo, &status) {
            println!("{warning}");
        }
    }

    Ok(())
}

fn cmd_doctor(root: PathBuf) -> Result<(), String> {
    require_dir(&root)?;
    let rules = Rules::load(&root)?;
    let db = db_path(&root);
    let objects = object_store_path(&root);
    let secrets = secret_store_path(&root);

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
        "object store: {}",
        if objects.is_dir() { "ok" } else { "missing" }
    );
    println!("daemon: not implemented");
    println!("remote manifest: not configured");
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

fn cmd_hydrate(path: PathBuf) -> Result<(), String> {
    if path.exists() {
        println!("already local: {}", display_path(&path));
        Ok(())
    } else {
        hydrate_from_local_store(&path)
    }
}

fn cmd_pin(path: PathBuf, pin: bool) -> Result<(), String> {
    let root = find_workspace_root(&path).ok_or_else(|| {
        "no .devdrop workspace found; run `devdrop workspace init <path>`".to_string()
    })?;
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

fn init_workspace_storage(root: &Path) -> Result<(), String> {
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

fn init_remote_storage(remote: &Path) -> Result<(), String> {
    fs::create_dir_all(remote_manifest_dir(remote))
        .map_err(|err| format!("create remote manifest dir: {err}"))?;
    fs::create_dir_all(remote_objects_path(remote))
        .map_err(|err| format!("create remote object dir: {err}"))?;
    fs::create_dir_all(remote_secrets_path(remote))
        .map_err(|err| format!("create remote secret dir: {err}"))?;
    Ok(())
}

fn workspace_root_for(path: &Path) -> Result<PathBuf, String> {
    require_dir(path)?;
    find_workspace_root(path).ok_or_else(|| {
        format!(
            "no .devdrop workspace found at or above {}",
            display_path(path)
        )
    })
}

fn db_path(root: &Path) -> PathBuf {
    root.join(".devdrop/devdrop.sqlite")
}

fn object_store_path(root: &Path) -> PathBuf {
    root.join(".devdrop/objects")
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

fn write_remote_config(root: &Path, remote: &Path) -> Result<(), String> {
    fs::write(
        remote_config_path(root),
        remote.to_string_lossy().as_bytes(),
    )
    .map_err(|err| format!("write remote config: {err}"))
}

fn read_remote_config(root: &Path) -> Result<Option<PathBuf>, String> {
    let path = remote_config_path(root);
    if !path.exists() {
        return Ok(None);
    }

    let text = fs::read_to_string(&path).map_err(|err| format!("read remote config: {err}"))?;
    let text = text.trim();
    Ok((!text.is_empty()).then(|| PathBuf::from(text)))
}

fn fetch_remote_object(root: &Path, hash: &str) -> Result<(), String> {
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

fn mark_node_local(root: &Path, rel: &str) -> Result<(), String> {
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

fn init_db(root: &Path) -> Result<(), String> {
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
";
    run_sql(&db, schema)
}

fn hydrate_from_local_store(path: &Path) -> Result<(), String> {
    let root = find_workspace_root(path).ok_or_else(|| {
        format!(
            "no .devdrop workspace found; cannot hydrate {}",
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

fn optional_path(arg: Option<&String>) -> Result<PathBuf, String> {
    arg.map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(|| env::current_dir().map_err(|err| format!("current dir: {err}")))
}

fn first_positional(args: &[String]) -> Option<&String> {
    let mut skip_next = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        match arg.as_str() {
            "--remote" => skip_next = true,
            "--pull" => {}
            _ if arg.starts_with("--") => {}
            _ => return Some(arg),
        }
    }
    None
}

fn required_path(arg: Option<&String>, command: &str) -> Result<PathBuf, String> {
    arg.map(PathBuf::from)
        .ok_or_else(|| format!("usage: devdrop {command} <path>"))
}

fn require_dir(path: &Path) -> Result<(), String> {
    if path.is_dir() {
        Ok(())
    } else {
        Err(format!("not a directory: {}", display_path(path)))
    }
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn rel_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn rel_or_dot(root: &Path, path: &Path) -> String {
    let rel = pin_path(root, path);
    if rel.is_empty() { ".".to_string() } else { rel }
}

fn find_workspace_root(path: &Path) -> Option<PathBuf> {
    let mut current = if path.is_dir() { path } else { path.parent()? };

    loop {
        if current.join(".devdrop").is_dir() {
            return Some(current.to_path_buf());
        }

        current = current.parent()?;
    }
}

fn pin_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn read_pins(root: &Path) -> Result<Vec<String>, String> {
    let path = root.join(".devdrop/pins");
    if !path.exists() {
        return Ok(Vec::new());
    }

    let text = fs::read_to_string(&path).map_err(|err| format!("read pins: {err}"))?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(String::from)
        .collect())
}

fn write_pins(path: &Path, pins: &[String]) -> Result<(), String> {
    let mut file = fs::File::create(path).map_err(|err| format!("write pins: {err}"))?;
    for pin in pins {
        writeln!(file, "{pin}").map_err(|err| format!("write pins: {err}"))?;
    }
    Ok(())
}

struct SecretRow {
    encrypted_path: PathBuf,
}

struct IndexedEntry {
    name: String,
    state: String,
    kind: String,
}

fn secret_store_path(root: &Path) -> PathBuf {
    root.join(".devdrop/secrets")
}

fn secret_cipher_path(root: &Path, rel: &str, scope: &str) -> PathBuf {
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

fn upsert_secret(
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
            "no local index; run `devdrop workspace init {}` first",
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

fn push_remote_secrets(root: &Path, remote: &Path) -> Result<(), String> {
    let db = db_path(root);
    let mut out = File::create(remote.join("secrets.tsv"))
        .map_err(|err| format!("write remote secrets manifest: {err}"))?;
    writeln!(out, "devdrop-secrets-v1").map_err(|err| format!("write secrets manifest: {err}"))?;
    if !db.exists() {
        return Ok(());
    }

    for row in query_lines(
        &db,
        "SELECT hex(path)||char(9)||hex(scope)||char(9)||hex(encrypted_path) FROM secrets ORDER BY path, scope;",
    )? {
        let fields = row.split('\t').collect::<Vec<_>>();
        if fields.len() != 3 {
            continue;
        }
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
        writeln!(
            out,
            "{}\t{}\t{}",
            hex_encode(rel.as_bytes()),
            hex_encode(scope.as_bytes()),
            hex_encode(name.as_bytes())
        )
        .map_err(|err| format!("write secrets manifest: {err}"))?;
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

fn locked_secret_count(root: &Path) -> Result<usize, String> {
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

fn indexed_state_count(root: &Path, state: &str) -> Result<usize, String> {
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

fn locked_secrets_in_dir(dir: &Path) -> Result<Vec<String>, String> {
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

fn indexed_entries_in_dir(dir: &Path) -> Result<Vec<IndexedEntry>, String> {
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

fn indexed_entry_from_row(row: &str, dir_rel: &str) -> Result<Option<IndexedEntry>, String> {
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

fn secret_env_for_repo(repo: &Path, scope: &str) -> Result<Vec<(String, String)>, String> {
    require_secret_key()?;
    require_dir(repo)?;
    let root = find_workspace_root(repo).ok_or_else(|| {
        "no .devdrop workspace found; run `devdrop workspace init <path>`".to_string()
    })?;
    let path = repo.join(".env");
    let rel = pin_path(&root, &path);
    let secret = lookup_secret(&root, &rel, scope)?;
    let plaintext = openssl_decrypt_to_string(&secret.encrypted_path)?;
    parse_env(&plaintext)
}

fn openssl_crypt(decrypt: bool, input: &Path, output: &Path) -> Result<(), String> {
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("create {}: {err}", display_path(parent)))?;
    }

    let mut command = Command::new("openssl");
    command.args([
        "enc",
        "-aes-256-cbc",
        "-pbkdf2",
        "-iter",
        "200000",
        "-salt",
        "-md",
        "sha256",
        "-pass",
        "env:DEVDROP_SECRET_KEY",
    ]);
    if decrypt {
        command.arg("-d");
    }
    command.arg("-in").arg(input).arg("-out").arg(output);
    run_command(command, "openssl").map(|_| ())
}

fn openssl_decrypt_to_string(input: &Path) -> Result<String, String> {
    let mut command = Command::new("openssl");
    command.args([
        "enc",
        "-aes-256-cbc",
        "-pbkdf2",
        "-iter",
        "200000",
        "-salt",
        "-md",
        "sha256",
        "-pass",
        "env:DEVDROP_SECRET_KEY",
        "-d",
    ]);
    command.arg("-in").arg(input);
    let bytes = run_command(command, "openssl")?;
    String::from_utf8(bytes).map_err(|err| format!("secret is not utf-8: {err}"))
}

fn run_command(mut command: Command, name: &str) -> Result<Vec<u8>, String> {
    let output = command
        .output()
        .map_err(|err| format!("run {name}: {err}"))?;

    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

fn parse_env(text: &str) -> Result<Vec<(String, String)>, String> {
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

fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|window| window[0] == flag)
        .map(|window| window[1].as_str())
}

fn json_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Action {
    Sync,
    Ignore,
    MetadataOnly,
    LocalOnly,
    Secret,
    HydrateOnAccess,
}

impl Action {
    fn from_token(token: &str) -> Option<Self> {
        match token {
            "sync" => Some(Self::Sync),
            "ignore" => Some(Self::Ignore),
            "metadata-only" => Some(Self::MetadataOnly),
            "local-only" => Some(Self::LocalOnly),
            "secret" => Some(Self::Secret),
            "hydrate-on-access" => Some(Self::HydrateOnAccess),
            _ => None,
        }
    }

    fn state_label(self) -> &'static str {
        match self {
            Self::Sync => "local",
            Self::Ignore => "ignored",
            Self::MetadataOnly => "metadata-only",
            Self::LocalOnly => "local-only",
            Self::Secret => "secret-locked",
            Self::HydrateOnAccess => "remote-only",
        }
    }

    fn token(self) -> &'static str {
        match self {
            Self::Sync => "sync",
            Self::Ignore => "ignore",
            Self::MetadataOnly => "metadata-only",
            Self::LocalOnly => "local-only",
            Self::Secret => "secret",
            Self::HydrateOnAccess => "hydrate-on-access",
        }
    }

    fn skips_children(self) -> bool {
        matches!(self, Self::Ignore | Self::MetadataOnly | Self::LocalOnly)
    }
}

#[derive(Debug)]
struct Rule {
    pattern: String,
    action: Action,
    dir_only: bool,
}

impl Rule {
    fn new(pattern: &str, action: Action) -> Self {
        let dir_only = pattern.ends_with('/');
        Self {
            pattern: pattern
                .trim_start_matches('/')
                .trim_end_matches('/')
                .to_string(),
            action,
            dir_only,
        }
    }

    fn parse(line: &str) -> Option<Self> {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            return None;
        }

        let mut fields = line.split_whitespace();
        let mut pattern = fields.next()?;
        let mut action = Action::Ignore;

        if let Some(stripped) = pattern.strip_prefix('!') {
            pattern = stripped;
            action = Action::Sync;
        }

        if let Some(token) = fields.next().and_then(Action::from_token) {
            action = token;
        }

        (!pattern.is_empty()).then(|| Self::new(pattern, action))
    }

    fn matches(&self, rel: &str, is_dir: bool) -> bool {
        let rel = rel.trim_start_matches("./");
        if rel.is_empty() {
            return false;
        }

        if self.dir_only {
            return self.matches_directory(rel, is_dir);
        }

        if self.pattern.contains('/') {
            wildcard_match(&self.pattern, rel)
        } else {
            rel.rsplit('/')
                .next()
                .is_some_and(|name| wildcard_match(&self.pattern, name))
        }
    }

    fn matches_directory(&self, rel: &str, is_dir: bool) -> bool {
        if self.pattern.contains('/') {
            return rel == self.pattern
                || rel
                    .strip_prefix(&self.pattern)
                    .is_some_and(|rest| rest.starts_with('/'));
        }

        let mut parts = rel.split('/').peekable();
        while let Some(part) = parts.next() {
            if wildcard_match(&self.pattern, part) && (is_dir || parts.peek().is_some()) {
                return true;
            }
        }

        false
    }
}

struct Rules {
    rules: Vec<Rule>,
    default_count: usize,
    custom_count: usize,
}

impl Rules {
    fn load(root: &Path) -> Result<Self, String> {
        let mut rules = default_rules();
        let default_count = rules.len();
        let mut custom_count = 0;
        let custom_path = root.join(".devsyncignore");

        if custom_path.exists() {
            let text = fs::read_to_string(&custom_path)
                .map_err(|err| format!("read {}: {err}", display_path(&custom_path)))?;
            for line in text.lines() {
                if let Some(rule) = Rule::parse(line) {
                    rules.push(rule);
                    custom_count += 1;
                }
            }
        }

        Ok(Self {
            rules,
            default_count,
            custom_count,
        })
    }

    fn action_for(&self, rel: &str, is_dir: bool) -> Action {
        self.rules
            .iter()
            .rev()
            .find(|rule| rule.matches(rel, is_dir))
            .map(|rule| rule.action)
            .unwrap_or(Action::Sync)
    }
}

fn default_rules() -> Vec<Rule> {
    [
        (".git/", Action::LocalOnly),
        (".devdrop/", Action::LocalOnly),
        ("node_modules/", Action::MetadataOnly),
        (".pnpm-store/", Action::LocalOnly),
        (".venv/", Action::LocalOnly),
        ("venv/", Action::LocalOnly),
        ("vendor/", Action::MetadataOnly),
        (".next/", Action::Ignore),
        (".nuxt/", Action::Ignore),
        (".turbo/", Action::Ignore),
        (".vite/", Action::Ignore),
        ("dist/", Action::Ignore),
        ("build/", Action::Ignore),
        ("target/", Action::LocalOnly),
        (".cache/", Action::Ignore),
        ("__pycache__/", Action::Ignore),
        (".pytest_cache/", Action::Ignore),
        (".mypy_cache/", Action::Ignore),
        (".gradle/", Action::LocalOnly),
        ("DerivedData/", Action::LocalOnly),
        (".build/", Action::LocalOnly),
        ("tmp/", Action::Ignore),
        ("*.pyc", Action::Ignore),
        ("*.log", Action::Ignore),
        (".DS_Store", Action::Ignore),
        (".env", Action::Secret),
        (".env.*", Action::Secret),
        ("*.pem", Action::Secret),
        ("*.key", Action::Secret),
        ("id_rsa", Action::Secret),
        ("id_ed25519", Action::Secret),
        (".env.example", Action::Sync),
    ]
    .into_iter()
    .map(|(pattern, action)| Rule::new(pattern, action))
    .collect()
}

#[derive(Default)]
struct Counts {
    entries: usize,
    local: usize,
    ignored: usize,
    metadata_only: usize,
    local_only: usize,
    remote_only: usize,
    secret_locked: usize,
    conflicted: usize,
    repos: usize,
    dirty_repos: usize,
}

struct IndexSnapshot {
    nodes: Vec<IndexNode>,
    blobs: HashMap<String, BlobRow>,
    repos: Vec<(String, RepoStatus)>,
}

struct IndexNode {
    id: String,
    parent_id: Option<String>,
    path: String,
    kind: String,
    mode: u32,
    size: u64,
    content_hash: Option<String>,
    local_state: String,
    local_mtime: i64,
}

struct BlobRow {
    hash: String,
    size: u64,
    local_path: String,
    ref_count: usize,
}

fn scan_workspace(root: &Path, rules: &Rules) -> Result<Counts, String> {
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

fn collect_index(root: &Path, rules: &Rules) -> Result<IndexSnapshot, String> {
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

fn push_index_node(
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

fn local_state_for(rel: &str, action: Action) -> &'static str {
    if is_conflict_path(rel) {
        "conflicted"
    } else {
        action.state_label()
    }
}

fn is_conflict_path(rel: &str) -> bool {
    rel.rsplit('/')
        .next()
        .is_some_and(|name| name.contains(" (conflict from ") && name.contains(')'))
}

fn write_index(root: &Path, rules: &Rules, snapshot: &IndexSnapshot) -> Result<(), String> {
    init_db(root)?;
    let db = db_path(root);
    let now = now_secs();
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

fn push_remote(root: &Path, remote: &Path, snapshot: &IndexSnapshot) -> Result<(), String> {
    init_remote_storage(remote)?;

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

    for blob in snapshot.blobs.values() {
        let dest = remote_object_path(remote, &blob.hash);
        if !dest.exists() {
            fs::copy(&blob.local_path, &dest)
                .map_err(|err| format!("copy remote blob {}: {err}", blob.hash))?;
        }
    }

    push_remote_secrets(root, remote)?;
    Ok(())
}

fn pull_remote(root: &Path, remote: &Path) -> Result<(), String> {
    init_workspace_storage(root)?;
    let nodes = read_remote_manifest(remote)?;

    for node in &nodes {
        if matches!(node.kind.as_str(), "directory" | "repo") && node.path != "." {
            fs::create_dir_all(root.join(&node.path))
                .map_err(|err| format!("create {}: {err}", node.path))?;
        }
    }

    pull_remote_secrets(root, remote)?;
    write_pulled_index(root, &nodes)?;
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
    let text = fs::read_to_string(remote_manifest_path(remote))
        .map_err(|err| format!("read remote manifest: {err}"))?;
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

fn walk_dirs<F>(root: &Path, path: &Path, rules: &Rules, visit: &mut F) -> Result<(), String>
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

fn find_repos(root: &Path, rules: &Rules) -> Result<Vec<PathBuf>, String> {
    let mut repos = Vec::new();

    if is_repo(root) {
        repos.push(root.to_path_buf());
    }

    walk_dirs(root, root, rules, &mut |path, file_type, action| {
        if file_type.is_dir() && is_repo(path) {
            repos.push(path.to_path_buf());
        }

        Ok(file_type.is_dir() && !action.skips_children())
    })?;

    repos.sort();
    repos.dedup();
    Ok(repos)
}

fn is_repo(path: &Path) -> bool {
    path.join(".git").exists()
}

fn repo_dirty(path: &Path) -> bool {
    git_output(path, &["status", "--porcelain"]).is_some_and(|text| !text.trim().is_empty())
}

struct RepoStatus {
    remote_url: Option<String>,
    branch: Option<String>,
    head: Option<String>,
    upstream: Option<String>,
    ahead: Option<usize>,
    behind: Option<usize>,
    dirty: bool,
}

fn repo_status(path: &Path) -> RepoStatus {
    let upstream = git_output(
        path,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    );
    let (ahead, behind) = upstream
        .as_ref()
        .and_then(|_| {
            git_output(
                path,
                &["rev-list", "--left-right", "--count", "HEAD...@{u}"],
            )
        })
        .and_then(|text| {
            let mut fields = text.split_whitespace();
            let ahead = fields.next()?.parse().ok()?;
            let behind = fields.next()?.parse().ok()?;
            Some((ahead, behind))
        })
        .unwrap_or((0, 0));

    RepoStatus {
        remote_url: git_output(path, &["config", "--get", "remote.origin.url"]),
        branch: git_output(path, &["rev-parse", "--abbrev-ref", "HEAD"]),
        head: git_output(path, &["rev-parse", "--short", "HEAD"]),
        upstream,
        ahead: Some(ahead),
        behind: Some(behind),
        dirty: repo_dirty(path),
    }
}

fn stale_repo_warning(path: &Path, status: &RepoStatus) -> Option<String> {
    let behind = status.behind?;
    (behind > 0).then(|| {
        format!(
            "warning: {} is {behind} commits behind {}. Run `devdrop repo update {}` before starting work.",
            display_path(path),
            status.upstream.as_deref().unwrap_or("upstream"),
            display_path(path)
        )
    })
}

fn run_git(path: &Path, args: &[&str]) -> Result<(), String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .map_err(|err| format!("run git: {err}"))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Err(if stderr.is_empty() { stdout } else { stderr })
    }
}

fn node_kind(path: &Path, action: Action, metadata: &fs::Metadata) -> String {
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
fn file_mode(metadata: &fs::Metadata) -> u32 {
    metadata.permissions().mode()
}

#[cfg(not(unix))]
fn file_mode(metadata: &fs::Metadata) -> u32 {
    if metadata.permissions().readonly() {
        0o444
    } else {
        0o666
    }
}

fn modified_secs(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn file_hash(path: &Path) -> Result<(String, u64), String> {
    let mut file = File::open(path).map_err(|err| format!("read {}: {err}", display_path(path)))?;
    let mut hash = 0xcbf29ce484222325u64;
    let mut size = 0;
    let mut buf = [0u8; 32 * 1024];

    loop {
        let read = file
            .read(&mut buf)
            .map_err(|err| format!("read {}: {err}", display_path(path)))?;
        if read == 0 {
            break;
        }

        size += read as u64;
        // ponytail: FNV is local cache identity only; replace with BLAKE3/SHA-256 before remote trust.
        for byte in &buf[..read] {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }

    Ok((format!("fnv1a64:{hash:016x}"), size))
}

fn object_path(root: &Path, hash: &str) -> PathBuf {
    object_store_path(root).join(hash.replace(':', "_"))
}

fn node_id(rel: &str) -> String {
    format!("node_{:016x}", fnv_bytes(rel.as_bytes()))
}

fn parent_rel(rel: &str) -> Option<String> {
    if rel == "." {
        None
    } else {
        rel.rsplit_once('/')
            .map(|(parent, _)| parent.to_string())
            .or_else(|| Some(".".to_string()))
    }
}

fn fnv_bytes(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn hex_decode(text: &str) -> Result<Vec<u8>, String> {
    if !text.len().is_multiple_of(2) {
        return Err("odd-length hex".into());
    }

    text.as_bytes()
        .chunks(2)
        .map(|chunk| Ok((hex_nibble(chunk[0])? << 4) | hex_nibble(chunk[1])?))
        .collect()
}

fn hex_decode_string(text: &str) -> Result<String, String> {
    String::from_utf8(hex_decode(text)?).map_err(|err| format!("bad utf-8 hex: {err}"))
}

fn hex_nibble(byte: u8) -> Result<u8, String> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(format!("invalid hex byte: {}", byte as char)),
    }
}

fn run_sql(db: &Path, sql: &str) -> Result<(), String> {
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

fn query_one(db: &Path, sql: &str) -> Result<Option<String>, String> {
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

fn query_lines(db: &Path, sql: &str) -> Result<Vec<String>, String> {
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

fn log_operation(
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

fn operation_sql(op_type: &str, path: &str, payload: &str, status: &str, now: i64) -> String {
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

fn sql_optional(value: Option<&str>) -> String {
    value.map(sql_string).unwrap_or_else(|| "NULL".to_string())
}

fn sql_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn git_output(path: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .ok()?;

    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn command_ok(command: &str, args: &[&str]) -> bool {
    Command::new(command)
        .args(args)
        .output()
        .is_ok_and(|output| output.status.success())
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    let pattern = pattern.as_bytes();
    let text = text.as_bytes();
    let (mut p, mut t) = (0, 0);
    let mut star = None;
    let mut star_text = 0;

    while t < text.len() {
        if p < pattern.len() && pattern[p] == text[t] {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            p += 1;
            star_text = t;
        } else if let Some(star_pos) = star {
            p = star_pos + 1;
            star_text += 1;
            t = star_text;
        } else {
            return false;
        }
    }

    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }

    p == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_matches_suffixes() {
        assert!(wildcard_match("*.pyc", "thing.pyc"));
        assert!(wildcard_match(".env.*", ".env.local"));
        assert!(!wildcard_match("*.pyc", "thing.py"));
    }

    #[test]
    fn default_rules_keep_env_example_syncable() {
        let rules = Rules {
            rules: default_rules(),
            default_count: 0,
            custom_count: 0,
        };

        assert_eq!(rules.action_for(".env", false), Action::Secret);
        assert_eq!(rules.action_for(".env.local", false), Action::Secret);
        assert_eq!(rules.action_for(".env.example", false), Action::Sync);
    }

    #[test]
    fn directory_rules_match_nested_components() {
        let rule = Rule::new("node_modules/", Action::MetadataOnly);

        assert!(rule.matches("work/api/node_modules", true));
        assert!(rule.matches("work/api/node_modules/react/index.js", false));
        assert!(!rule.matches("work/api/not_node_modules", true));
    }

    #[test]
    fn later_rules_override_earlier_rules() {
        let rules = Rules {
            rules: vec![
                Rule::new("dist/", Action::Ignore),
                Rule::new("dist/", Action::Sync),
            ],
            default_count: 0,
            custom_count: 0,
        };

        assert_eq!(rules.action_for("dist", true), Action::Sync);
    }

    #[test]
    fn conflict_names_are_detected() {
        assert!(is_conflict_path(
            "src/config (conflict from Mac Mini 2026-06-23 10-41).ts"
        ));
        assert!(!is_conflict_path("src/conflict-free.ts"));
    }

    #[test]
    fn sql_strings_escape_quotes() {
        assert_eq!(sql_string("it's fine"), "'it''s fine'");
    }

    #[test]
    fn env_parser_handles_comments_exports_and_quotes() {
        let envs = parse_env(
            r#"
# comment
export API_KEY="abc123"
PLAIN=value
"#,
        )
        .unwrap();

        assert_eq!(
            envs,
            vec![
                ("API_KEY".to_string(), "abc123".to_string()),
                ("PLAIN".to_string(), "value".to_string())
            ]
        );
    }

    #[test]
    fn stale_repo_warning_mentions_upstream() {
        let status = RepoStatus {
            remote_url: None,
            branch: Some("main".into()),
            head: Some("abc123".into()),
            upstream: Some("origin/main".into()),
            ahead: Some(0),
            behind: Some(3),
            dirty: false,
        };
        let warning = stale_repo_warning(Path::new("/tmp/work/api"), &status).unwrap();

        assert!(warning.contains("3 commits behind origin/main"));
    }
}
