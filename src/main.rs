use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
        Some("login") => cmd_login(args.get(1)),
        Some("workspace") => cmd_workspace(&args[1..]),
        Some("device") => cmd_device(&args[1..]),
        Some("repo") => cmd_repo(&args[1..]),
        Some("remote") => cmd_remote(&args[1..]),
        Some("secret") => cmd_secret(&args[1..]),
        Some("agent") => cmd_agent(&args[1..]),
        Some("overlay") => cmd_overlay(&args[1..]),
        Some("run") => cmd_run(&args[1..]),
        Some("daemon") => cmd_daemon(&args[1..]),
        Some("sync") => cmd_sync(&args[1..]),
        Some("status") => cmd_status(&args[1..]),
        Some("ls") => cmd_ls(optional_path(args.get(1))?),
        Some("ignored") => cmd_ignored(optional_path(args.get(1))?),
        Some("conflicts") => cmd_conflicts(&args[1..]),
        Some("repo-status") => cmd_repo_status(&args[1..]),
        Some("doctor") => cmd_doctor(optional_path(args.get(1))?),
        Some("hydrate") => cmd_hydrate(required_path(args.get(1), "hydrate")?),
        Some("history") => cmd_history(required_path(args.get(1), "history")?),
        Some("recover") => cmd_recover(&args[1..]),
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
  devdrop login [user]
  devdrop workspace init|mount <path>
  devdrop device enroll <name>
  devdrop device list
  devdrop repo update [path]
  devdrop remote init <path>
  devdrop secret add <path> --scope <scope>
  devdrop secret request <path> [--scope <scope>]
  devdrop secret unlock <path> [--scope <scope>]
  devdrop secret lock <path> [--scope <scope>]
  devdrop agent create --repo <path> [--write-scope <scope>] [--secret-scope <scope>]
  devdrop agent status
  devdrop agent diff <agent-id>
  devdrop agent accept <agent-id>
  devdrop agent reject <agent-id>
  devdrop overlay diff [agent-id]
  devdrop overlay submit [agent-id]
  devdrop run --repo <path> [--secret-scope <scope>] -- <command>
  devdrop daemon [path] [--remote <path>] [--interval <seconds>] [--once]
  devdrop sync [path] [--remote <path>] [--pull]
  devdrop status [path] [--json]
  devdrop ls [path]
  devdrop ignored [path]
  devdrop conflicts [path]
  devdrop conflicts resolve <path> --use base|conflict
  devdrop repo-status [path] [--json]
  devdrop hydrate <path>
  devdrop history <path>
  devdrop recover <path> [--hash <content-hash>]
  devdrop pin <path>
  devdrop unpin <path>
  devdrop doctor [path]"
    );
}

fn cmd_login(user: Option<&String>) -> Result<(), String> {
    let cwd = env::current_dir().map_err(|err| format!("current dir: {err}"))?;
    let existing = find_workspace_root(&cwd);
    // Don't silently create a workspace — tell the user what's happening.
    if existing.is_none() {
        println!("no workspace found; initializing at {}", display_path(&cwd));
    }
    let root = existing.unwrap_or(cwd);

    init_workspace_storage(&root)?;
    let user = user
        .cloned()
        .or_else(|| env::var("USER").ok())
        .unwrap_or_else(|| "local".to_string());
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

fn cmd_workspace(args: &[String]) -> Result<(), String> {
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

fn cmd_device(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("enroll") => {
            let name = required_arg(args.get(1), "device enroll")?;
            cmd_device_enroll(name)
        }
        Some("list") => cmd_device_list(),
        _ => Err("usage: devdrop device enroll <name> | devdrop device list".into()),
    }
}

fn cmd_device_enroll(name: &str) -> Result<(), String> {
    let root = env::current_dir()
        .map_err(|err| format!("current dir: {err}"))
        .ok()
        .and_then(|dir| find_workspace_root(&dir))
        .ok_or_else(|| "no .devdrop workspace found; run inside a workspace".to_string())?;
    init_db(&root)?;
    let user = current_user(&root)?.unwrap_or_else(|| "local".to_string());
    upsert_user(&root, &user)?;
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
        &db_path(&root),
        &format!(
            "INSERT OR REPLACE INTO devices (id, workspace_id, user_id, name, os, arch, trust_level, last_seen_at, public_key) VALUES ({}, 'local', {}, {}, {}, {}, 'personal', {}, {});\n",
            sql_string(&id),
            sql_string(&user),
            sql_string(name),
            sql_string(current_os()),
            sql_string(current_arch()),
            now,
            sql_string(&public_key)
        ),
    )?;
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

fn cmd_device_list() -> Result<(), String> {
    let root = env::current_dir()
        .map_err(|err| format!("current dir: {err}"))
        .ok()
        .and_then(|dir| find_workspace_root(&dir))
        .ok_or_else(|| "no .devdrop workspace found; run inside a workspace".to_string())?;
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

fn cmd_agent(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("create") => {
            let repo = flag_value(args, "--repo")
                .map(PathBuf::from)
                .ok_or_else(|| "usage: devdrop agent create --repo <path>".to_string())?;
            let write_scope = flag_value(args, "--write-scope").unwrap_or("**");
            let secret_scope = flag_value(args, "--secret-scope").unwrap_or("");
            cmd_agent_create(&repo, write_scope, secret_scope)
        }
        Some("status") => cmd_agent_status(),
        Some("diff") => cmd_agent_diff(required_arg(args.get(1), "agent diff")?),
        Some("accept") => cmd_agent_finish(required_arg(args.get(1), "agent accept")?, true),
        Some("reject") => cmd_agent_finish(required_arg(args.get(1), "agent reject")?, false),
        _ => Err("usage: devdrop agent create|status|diff|accept|reject".into()),
    }
}

fn cmd_overlay(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("diff") => {
            let (_, agent) = find_overlay_agent(args.get(1).map(String::as_str))?;
            cmd_agent_diff(&agent.id)
        }
        Some("submit") => cmd_overlay_submit(args.get(1).map(String::as_str)),
        _ => Err("usage: devdrop overlay diff|submit [agent-id]".into()),
    }
}

fn cmd_agent_create(repo: &Path, write_scope: &str, secret_scope: &str) -> Result<(), String> {
    require_dir(repo)?;
    let root = find_workspace_root(repo).ok_or_else(|| {
        "no .devdrop workspace found; run `devdrop workspace init <path>`".to_string()
    })?;
    init_db(&root)?;
    let id = format!("agent_{}", now_nanos());
    let overlay = agent_overlay_path(&root, &id);
    let base = agent_base_path(&root, &id);
    copy_tree(repo, &base)?;
    copy_tree(&base, &overlay)?;
    upsert_agent(
        &root,
        &id,
        repo,
        &overlay,
        write_scope,
        secret_scope,
        "pending",
    )?;
    log_operation(
        &root,
        "agent_create",
        &pin_path(&root, repo),
        &format!("{{\"agent\":{}}}", json_string(&id)),
        "done",
    )?;
    println!("agent created: {id}");
    println!("overlay: {}", display_path(&overlay));
    Ok(())
}

fn cmd_agent_status() -> Result<(), String> {
    let root = env::current_dir()
        .map_err(|err| format!("current dir: {err}"))
        .ok()
        .and_then(|dir| find_workspace_root(&dir))
        .ok_or_else(|| "no .devdrop workspace found".to_string())?;
    let agents = list_agents(&root)?;
    if agents.is_empty() {
        println!("no agent workspaces");
        return Ok(());
    }

    for agent in agents {
        println!(
            "{} status={} repo={} overlay={} write_scope={} secret_scope={}",
            agent.id,
            agent.status,
            agent.repo_path,
            agent.overlay_path,
            agent.write_scope,
            if agent.secret_scope.is_empty() {
                "none"
            } else {
                &agent.secret_scope
            }
        );
    }
    Ok(())
}

fn cmd_agent_diff(id: &str) -> Result<(), String> {
    let (root, agent) = find_agent(id)?;
    ensure_agent_reviewable(&agent)?;

    // Show scope violations before the diff so the user knows what will be
    // rejected at accept time.
    let violations = agent_scope_violations(&root, &agent)?;
    if !violations.is_empty() {
        eprintln!(
            "\u{1b}[33mwarning: {} path(s) outside write scope `{}`:\u{1b}[0m",
            violations.len(),
            agent.write_scope
        );
        for path in &violations {
            eprintln!("  {path}");
        }
        eprintln!();
    }

    // Exclude .git and .devdrop — they are intentionally absent from the
    // overlay (skip_overlay_component filters them during copy_tree).
    // Without --exclude, diff floods with "Only in ./.git/..." noise.
    let output = Command::new("diff")
        .args(["-ruN", "--exclude=.git", "--exclude=.devdrop"])
        .arg(&agent.repo_path)
        .arg(&agent.overlay_path)
        .output()
        .map_err(|err| format!("run diff: {err}"))?;
    print!("{}", String::from_utf8_lossy(&output.stdout));
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    if output.status.code() == Some(2) {
        return Err("diff failed".into());
    }
    log_operation(
        &root,
        "agent_diff",
        &agent.repo_path,
        &format!("{{\"agent\":{}}}", json_string(id)),
        "done",
    )?;
    Ok(())
}

fn cmd_agent_finish(id: &str, accept: bool) -> Result<(), String> {
    let (root, agent) = find_agent(id)?;
    ensure_agent_reviewable(&agent)?;
    if accept {
        let violations = agent_scope_violations(&root, &agent)?;
        if !violations.is_empty() {
            log_operation(
                &root,
                "agent_accept_denied",
                &agent.repo_path,
                &format!(
                    "{{\"agent\":{},\"path\":{}}}",
                    json_string(id),
                    json_string(&violations[0])
                ),
                "blocked",
            )?;
            return Err(format!(
                "overlay changes outside write scope `{}`: {}",
                agent.write_scope,
                violations.join(", ")
            ));
        }
        let stale = agent_stale_paths(&root, &agent)?;
        if !stale.is_empty() {
            log_operation(
                &root,
                "agent_accept_stale",
                &agent.repo_path,
                &format!(
                    "{{\"agent\":{},\"path\":{}}}",
                    json_string(id),
                    json_string(&stale[0])
                ),
                "blocked",
            )?;
            return Err(format!(
                "repo changed since overlay was created: {}",
                stale.join(", ")
            ));
        }
        sync_tree(Path::new(&agent.overlay_path), Path::new(&agent.repo_path))?;
    }
    let status = if accept { "accepted" } else { "rejected" };
    update_agent_status(&root, id, status)?;
    log_operation(
        &root,
        if accept {
            "agent_accept"
        } else {
            "agent_reject"
        },
        &agent.repo_path,
        &format!("{{\"agent\":{}}}", json_string(id)),
        "done",
    )?;
    println!("agent {status}: {id}");
    Ok(())
}

fn cmd_overlay_submit(id: Option<&str>) -> Result<(), String> {
    let (root, agent) = find_overlay_agent(id)?;
    if agent.status != "pending" && agent.status != "submitted" {
        return Err(format!("agent {} is {}", agent.id, agent.status));
    }
    update_agent_status(&root, &agent.id, "submitted")?;
    log_operation(
        &root,
        "overlay_submit",
        &agent.repo_path,
        &format!("{{\"agent\":{}}}", json_string(&agent.id)),
        "done",
    )?;
    println!("overlay submitted: {}", agent.id);
    Ok(())
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

fn cmd_daemon(args: &[String]) -> Result<(), String> {
    let root = optional_path(first_positional(args))?;
    let root = workspace_root_for(&root)?;
    let remote = flag_value(args, "--remote")
        .map(PathBuf::from)
        .or_else(|| read_remote_config(&root).ok().flatten());
    let interval = flag_value(args, "--interval")
        .unwrap_or("2")
        .parse::<u64>()
        .map_err(|err| format!("invalid --interval: {err}"))?
        .max(1);
    let once = args.iter().any(|arg| arg == "--once");
    let mut last_signature = None;

    println!(
        "daemon watching: {} interval={}s{}",
        display_path(&root),
        interval,
        remote
            .as_ref()
            .map(|path| format!(" remote={}", display_path(path)))
            .unwrap_or_default()
    );

    loop {
        let rules = Rules::load(&root)?;
        let snapshot = collect_index(&root, &rules)?;
        let signature = snapshot_signature(&snapshot);

        if last_signature != Some(signature) {
            write_index(&root, &rules, &snapshot)?;
            if let Some(remote) = &remote {
                push_remote(&root, remote, &snapshot)?;
                write_remote_config(&root, remote)?;
            }
            println!(
                "daemon synced: nodes={} blobs={} repos={}",
                snapshot.nodes.len(),
                snapshot.blobs.len(),
                snapshot.repos.len()
            );
            last_signature = Some(signature);
        } else if once {
            println!("daemon scan: no changes");
        }

        if once {
            return Ok(());
        }

        // ponytail: polling watcher, replace with notify/FSEvents/inotify when dependency policy allows.
        thread::sleep(Duration::from_secs(interval));
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

fn cmd_secret_request(path: &Path, scope: &str) -> Result<(), String> {
    require_secret_key()?;
    let root = find_workspace_root(path).ok_or_else(|| {
        "no .devdrop workspace found; run `devdrop workspace init <path>`".to_string()
    })?;
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
    let mut snapshot = collect_index(root, &rules)?;
    carry_indexed_remote_nodes(root, &mut snapshot)?;
    write_index(root, &rules, &snapshot)?;
    Ok(snapshot)
}

fn snapshot_signature(snapshot: &IndexSnapshot) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    let mut nodes = snapshot.nodes.iter().collect::<Vec<_>>();
    nodes.sort_by(|left, right| left.path.cmp(&right.path));

    for node in nodes {
        for part in [
            node.path.as_str(),
            node.kind.as_str(),
            node.local_state.as_str(),
            node.content_hash.as_deref().unwrap_or(""),
        ] {
            hash ^= fnv_bytes(part.as_bytes());
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash ^= node.size;
        hash = hash.wrapping_mul(0x100000001b3);
        hash ^= node.local_mtime as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }

    hash
}

fn cmd_status(args: &[String]) -> Result<(), String> {
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

fn cmd_conflicts(args: &[String]) -> Result<(), String> {
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
        .ok_or_else(|| {
            "no .devdrop workspace found; run `devdrop workspace init <path>`".to_string()
        })?;

    match choice {
        "base" | "ours" => {
            archive_conflict_file(&root, &pair.conflict)?;
        }
        "conflict" | "theirs" => {
            archive_conflict_file(&root, &pair.base)?;
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

fn conflict_base_path(path: &Path) -> Result<PathBuf, String> {
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

fn cmd_repo_status(args: &[String]) -> Result<(), String> {
    let root = optional_path(first_positional(args))?;
    let json = args.iter().any(|arg| arg == "--json");
    require_dir(&root)?;
    let rules = Rules::load(&root)?;
    let repos = find_repos(&root, &rules)?;

    if json {
        print!("[");
        for (index, repo) in repos.iter().enumerate() {
            let status = repo_status(repo);
            if index > 0 {
                print!(",");
            }
            print_repo_status_json(repo, &status);
        }
        println!("]");
        return Ok(());
    }

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
            .map(|path| format!("configured {}", display_path(path)))
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

fn cmd_hydrate(path: PathBuf) -> Result<(), String> {
    if path.exists() {
        println!("already local: {}", display_path(&path));
        Ok(())
    } else {
        hydrate_from_local_store(&path)
    }
}

fn cmd_history(path: PathBuf) -> Result<(), String> {
    let root = find_workspace_root(&path)
        .or_else(|| path.parent().and_then(find_workspace_root))
        .ok_or_else(|| {
            "no .devdrop workspace found; run `devdrop workspace init <path>`".to_string()
        })?;
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

fn cmd_recover(args: &[String]) -> Result<(), String> {
    let path = required_path(args.first(), "recover")?;
    let root = find_workspace_root(&path)
        .or_else(|| path.parent().and_then(find_workspace_root))
        .ok_or_else(|| {
            "no .devdrop workspace found; run `devdrop workspace init <path>`".to_string()
        })?;
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

fn remote_devices_path(remote: &Path) -> PathBuf {
    remote.join("devices.tsv")
}

fn remote_tombstones_path(remote: &Path) -> PathBuf {
    remote.join("tombstones.tsv")
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

fn upsert_user(root: &Path, user: &str) -> Result<(), String> {
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

fn current_user(root: &Path) -> Result<Option<String>, String> {
    let db = db_path(root);
    if !db.exists() {
        return Ok(None);
    }
    query_one(
        &db,
        "SELECT id FROM users WHERE workspace_id='local' ORDER BY logged_in_at DESC LIMIT 1;",
    )
}

fn device_count(root: &Path) -> Result<usize, String> {
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

fn push_remote_devices(root: &Path, remote: &Path) -> Result<(), String> {
    let mut out = File::create(remote_devices_path(remote))
        .map_err(|err| format!("write remote devices manifest: {err}"))?;
    writeln!(out, "devdrop-devices-v1").map_err(|err| format!("write devices manifest: {err}"))?;

    let db = db_path(root);
    if !db.exists() {
        return Ok(());
    }

    for row in query_lines(
        &db,
        "SELECT hex(id)||char(9)||hex(user_id)||char(9)||hex(name)||char(9)||hex(os)||char(9)||hex(arch)||char(9)||hex(trust_level)||char(9)||last_seen_at||char(9)||hex(public_key) FROM devices ORDER BY id;",
    )? {
        writeln!(out, "{row}").map_err(|err| format!("write devices manifest: {err}"))?;
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

fn current_os() -> &'static str {
    match env::consts::OS {
        "macos" => "macos",
        "windows" => "windows",
        _ => "linux",
    }
}

fn current_arch() -> &'static str {
    match env::consts::ARCH {
        "x86_64" | "x86" => "x64",
        "aarch64" => "arm64",
        _ => env::consts::ARCH,
    }
}

#[derive(Clone)]
struct AgentRow {
    id: String,
    repo_path: String,
    overlay_path: String,
    write_scope: String,
    secret_scope: String,
    status: String,
}

fn agent_overlay_path(root: &Path, id: &str) -> PathBuf {
    root.join(".devdrop/agents").join(id).join("overlay")
}

fn agent_base_path(root: &Path, id: &str) -> PathBuf {
    root.join(".devdrop/agents").join(id).join("base")
}

fn upsert_agent(
    root: &Path,
    id: &str,
    repo: &Path,
    overlay: &Path,
    write_scope: &str,
    secret_scope: &str,
    status: &str,
) -> Result<(), String> {
    let now = now_secs();
    run_sql(
        &db_path(root),
        &format!(
            "INSERT OR REPLACE INTO agents (id, workspace_id, repo_path, overlay_path, write_scope, secret_scope, status, created_at, updated_at) VALUES ({}, 'local', {}, {}, {}, {}, {}, {}, {});\n",
            sql_string(id),
            sql_string(&repo.to_string_lossy()),
            sql_string(&overlay.to_string_lossy()),
            sql_string(write_scope),
            sql_string(secret_scope),
            sql_string(status),
            now,
            now
        ),
    )
}

fn update_agent_status(root: &Path, id: &str, status: &str) -> Result<(), String> {
    run_sql(
        &db_path(root),
        &format!(
            "UPDATE agents SET status={}, updated_at={} WHERE id={};\n",
            sql_string(status),
            now_secs(),
            sql_string(id)
        ),
    )
}

fn list_agents(root: &Path) -> Result<Vec<AgentRow>, String> {
    init_db(root)?;
    query_lines(
        &db_path(root),
        "SELECT hex(id)||char(9)||hex(repo_path)||char(9)||hex(overlay_path)||char(9)||hex(write_scope)||char(9)||hex(secret_scope)||char(9)||hex(status) FROM agents ORDER BY created_at, id;",
    )?
    .into_iter()
    .map(|line| parse_agent_row(&line))
    .collect()
}

fn parse_agent_row(line: &str) -> Result<AgentRow, String> {
    let fields = line.split('\t').collect::<Vec<_>>();
    if fields.len() != 6 {
        return Err("bad agent row".into());
    }
    Ok(AgentRow {
        id: hex_decode_string(fields[0])?,
        repo_path: hex_decode_string(fields[1])?,
        overlay_path: hex_decode_string(fields[2])?,
        write_scope: hex_decode_string(fields[3])?,
        secret_scope: hex_decode_string(fields[4])?,
        status: hex_decode_string(fields[5])?,
    })
}

fn find_agent(id: &str) -> Result<(PathBuf, AgentRow), String> {
    let cwd = env::current_dir().map_err(|err| format!("current dir: {err}"))?;
    let root =
        find_workspace_root(&cwd).ok_or_else(|| "no .devdrop workspace found".to_string())?;
    let agent = list_agents(&root)?
        .into_iter()
        .find(|agent| agent.id == id)
        .ok_or_else(|| format!("agent not found: {id}"))?;
    Ok((root, agent))
}

fn find_overlay_agent(id: Option<&str>) -> Result<(PathBuf, AgentRow), String> {
    if let Some(id) = id {
        return find_agent(id);
    }

    let cwd = env::current_dir().map_err(|err| format!("current dir: {err}"))?;
    let root =
        find_workspace_root(&cwd).ok_or_else(|| "no .devdrop workspace found".to_string())?;
    let agents = list_agents(&root)?;
    let cwd = fs::canonicalize(&cwd).unwrap_or(cwd);

    if let Some(agent) = agents.iter().find(|agent| {
        fs::canonicalize(&agent.overlay_path).is_ok_and(|overlay| cwd.starts_with(overlay))
    }) {
        return Ok((root, agent.clone()));
    }

    let pending = agents
        .into_iter()
        .filter(|agent| agent.status == "pending" || agent.status == "submitted")
        .collect::<Vec<_>>();
    match pending.as_slice() {
        [agent] => Ok((root, agent.clone())),
        [] => Err("no pending agent overlay; pass <agent-id>".into()),
        _ => Err("multiple pending agent overlays; pass <agent-id>".into()),
    }
}

fn ensure_agent_reviewable(agent: &AgentRow) -> Result<(), String> {
    if agent.status == "pending" || agent.status == "submitted" {
        Ok(())
    } else {
        Err(format!("agent {} is {}", agent.id, agent.status))
    }
}

// ponytail: full tree scan at accept time; use indexed overlay diffs when repos get large.
fn agent_scope_violations(root: &Path, agent: &AgentRow) -> Result<Vec<String>, String> {
    let base = agent_base_signature(root, agent)?;
    let overlay = tree_signature(Path::new(&agent.overlay_path))?;
    let mut paths = base
        .keys()
        .chain(overlay.keys())
        .cloned()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    paths.sort();

    Ok(paths
        .into_iter()
        .filter(|path| base.get(path) != overlay.get(path))
        .filter(|path| {
            let is_dir = base
                .get(path)
                .or_else(|| overlay.get(path))
                .is_some_and(|entry| entry == "dir");
            !write_scope_allows(&agent.write_scope, path, is_dir)
        })
        .collect())
}

fn agent_stale_paths(root: &Path, agent: &AgentRow) -> Result<Vec<String>, String> {
    let base_path = agent_base_path(root, &agent.id);
    if !base_path.is_dir() {
        return Ok(Vec::new());
    }
    let base = tree_signature(&base_path)?;
    let repo = tree_signature(Path::new(&agent.repo_path))?;
    let overlay = tree_signature(Path::new(&agent.overlay_path))?;
    let mut paths = base
        .keys()
        .chain(repo.keys())
        .chain(overlay.keys())
        .cloned()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    paths.sort();

    Ok(paths
        .into_iter()
        .filter(|path| repo.get(path) != base.get(path))
        .filter(|path| repo.get(path) != overlay.get(path))
        .collect())
}

fn agent_base_signature(root: &Path, agent: &AgentRow) -> Result<HashMap<String, String>, String> {
    let base = agent_base_path(root, &agent.id);
    if base.is_dir() {
        tree_signature(&base)
    } else {
        tree_signature(Path::new(&agent.repo_path))
    }
}

fn write_scope_allows(scope: &str, rel: &str, is_dir: bool) -> bool {
    scope
        .split(',')
        .flat_map(str::split_whitespace)
        .filter(|pattern| !pattern.is_empty())
        .any(|pattern| {
            pattern == "*"
                || pattern == "**"
                || pattern.strip_suffix("/**").is_some_and(|prefix| {
                    rel == prefix
                        || rel
                            .strip_prefix(prefix)
                            .is_some_and(|rest| rest.starts_with('/'))
                })
                || Rule::new(pattern, Action::Sync).matches(rel, is_dir)
        })
}

fn tree_signature(root: &Path) -> Result<HashMap<String, String>, String> {
    require_dir(root)?;
    let mut signature = HashMap::new();
    collect_tree_signature(root, root, &mut signature)?;
    Ok(signature)
}

fn collect_tree_signature(
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

fn copy_tree(src: &Path, dest: &Path) -> Result<(), String> {
    require_dir(src)?;
    fs::create_dir_all(dest).map_err(|err| format!("create {}: {err}", display_path(dest)))?;
    copy_tree_inner(src, dest, src)
}

fn sync_tree(src: &Path, dest: &Path) -> Result<(), String> {
    require_dir(src)?;
    fs::create_dir_all(dest).map_err(|err| format!("create {}: {err}", display_path(dest)))?;
    prune_missing(src, dest, dest)?;
    copy_tree(src, dest)
}

fn prune_missing(src_root: &Path, dest_root: &Path, current_dest: &Path) -> Result<(), String> {
    for entry in fs::read_dir(current_dest)
        .map_err(|err| format!("read {}: {err}", display_path(current_dest)))?
    {
        let entry = entry.map_err(|err| format!("read {}: {err}", display_path(current_dest)))?;
        let dest = entry.path();
        let rel = dest
            .strip_prefix(dest_root)
            .map_err(|err| format!("relative path: {err}"))?;
        if skip_overlay_component(rel) {
            continue;
        }
        let src = src_root.join(rel);
        let file_type = entry
            .file_type()
            .map_err(|err| format!("stat {}: {err}", display_path(&dest)))?;
        if !src.exists() {
            if file_type.is_dir() {
                fs::remove_dir_all(&dest)
                    .map_err(|err| format!("remove {}: {err}", display_path(&dest)))?;
            } else {
                fs::remove_file(&dest)
                    .map_err(|err| format!("remove {}: {err}", display_path(&dest)))?;
            }
        } else if file_type.is_dir() {
            prune_missing(src_root, dest_root, &dest)?;
        }
    }
    Ok(())
}

fn copy_tree_inner(root: &Path, dest_root: &Path, current: &Path) -> Result<(), String> {
    for entry in
        fs::read_dir(current).map_err(|err| format!("read {}: {err}", display_path(current)))?
    {
        let entry = entry.map_err(|err| format!("read {}: {err}", display_path(current)))?;
        let src = entry.path();
        let rel = src
            .strip_prefix(root)
            .map_err(|err| format!("relative path: {err}"))?;
        if skip_overlay_component(rel) {
            continue;
        }
        let dest = dest_root.join(rel);
        let file_type = entry
            .file_type()
            .map_err(|err| format!("stat {}: {err}", display_path(&src)))?;
        if file_type.is_dir() {
            fs::create_dir_all(&dest)
                .map_err(|err| format!("create {}: {err}", display_path(&dest)))?;
            copy_tree_inner(root, dest_root, &src)?;
        } else if file_type.is_file() {
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)
                    .map_err(|err| format!("create {}: {err}", display_path(parent)))?;
            }
            fs::copy(&src, &dest).map_err(|err| format!("copy {}: {err}", display_path(&src)))?;
        }
    }
    Ok(())
}

fn skip_overlay_component(rel: &Path) -> bool {
    rel.components().next().is_some_and(|part| {
        let part = part.as_os_str();
        part == ".git" || part == ".devdrop"
    })
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
            "--interval" | "--remote" | "--repo" | "--scope" | "--secret-scope" => skip_next = true,
            "--json" | "--once" | "--pull" => {}
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

fn required_arg<'a>(arg: Option<&'a String>, command: &str) -> Result<&'a str, String> {
    arg.map(String::as_str)
        .ok_or_else(|| format!("usage: devdrop {command} <id>"))
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

fn json_optional(value: Option<&str>) -> String {
    value.map(json_string).unwrap_or_else(|| "null".to_string())
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

struct PreviousFile {
    path: String,
    content_hash: String,
}

struct FileVersion {
    content_hash: String,
    size: u64,
    seen_at: i64,
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

fn carry_indexed_remote_nodes(root: &Path, snapshot: &mut IndexSnapshot) -> Result<(), String> {
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

fn previous_index_files(root: &Path) -> Result<Vec<PreviousFile>, String> {
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

fn file_version_sql(path: &str, hash: &str, size: u64, local_path: &Path, seen_at: i64) -> String {
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

fn file_versions(root: &Path, rel: &str) -> Result<Vec<FileVersion>, String> {
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

fn latest_file_version_hash(root: &Path, rel: &str) -> Result<Option<String>, String> {
    init_db(root)?;
    query_one(
        &db_path(root),
        &format!(
            "SELECT content_hash FROM file_versions WHERE workspace_id='local' AND path={} ORDER BY seen_at DESC LIMIT 1;",
            sql_string(rel)
        ),
    )
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
    push_remote_devices(root, remote)?;
    push_remote_tombstones(root, remote)?;
    Ok(())
}

fn pull_remote(root: &Path, remote: &Path) -> Result<(), String> {
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

struct RemoteTombstone {
    path: String,
    content_hash: Option<String>,
    deleted_at: i64,
}

fn push_remote_tombstones(root: &Path, remote: &Path) -> Result<(), String> {
    let mut out = File::create(remote_tombstones_path(remote))
        .map_err(|err| format!("write remote tombstones: {err}"))?;
    writeln!(out, "devdrop-tombstones-v1").map_err(|err| format!("write tombstones: {err}"))?;

    let db = db_path(root);
    if !db.exists() {
        return Ok(());
    }

    for row in query_lines(
        &db,
        "SELECT hex(path)||char(9)||ifnull(hex(content_hash),'')||char(9)||deleted_at FROM tombstones ORDER BY deleted_at, path;",
    )? {
        writeln!(out, "{row}").map_err(|err| format!("write tombstones: {err}"))?;
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

fn print_repo_status_json(path: &Path, status: &RepoStatus) {
    print!(
        "{{\"path\":{},\"remote\":{},\"branch\":{},\"head\":{},\"upstream\":{},\"ahead\":{},\"behind\":{},\"dirty\":{}}}",
        json_string(&display_path(path)),
        json_optional(status.remote_url.as_deref()),
        json_optional(status.branch.as_deref()),
        json_optional(status.head.as_deref()),
        json_optional(status.upstream.as_deref()),
        status.ahead.unwrap_or(0),
        status.behind.unwrap_or(0),
        status.dirty
    );
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

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
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
    fn conflict_base_path_removes_marker() {
        assert_eq!(
            conflict_base_path(Path::new(
                "src/config (conflict from Mac Mini 2026-06-23 10-41).ts"
            ))
            .unwrap(),
            PathBuf::from("src/config.ts")
        );
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

    #[test]
    fn write_scope_double_star_includes_root_directory() {
        assert!(write_scope_allows("src/**", "src", true));
        assert!(write_scope_allows("src/**", "src/lib.rs", false));
        assert!(!write_scope_allows("src/**", "tests/lib.rs", false));
    }

    #[test]
    fn agent_scope_violations_report_out_of_scope_changes() {
        let root = env::temp_dir().join(format!("devdrop-test-{}", now_nanos()));
        let repo = root.join("repo");
        let overlay = root.join("overlay");
        let base = agent_base_path(&root, "agent_test");
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::write(repo.join("src/app.rs"), "one\n").unwrap();
        fs::write(repo.join("README.md"), "docs\n").unwrap();
        copy_tree(&repo, &base).unwrap();
        copy_tree(&repo, &overlay).unwrap();
        fs::write(overlay.join("src/app.rs"), "two\n").unwrap();
        fs::write(overlay.join("README.md"), "changed\n").unwrap();

        let agent = AgentRow {
            id: "agent_test".into(),
            repo_path: repo.to_string_lossy().into_owned(),
            overlay_path: overlay.to_string_lossy().into_owned(),
            write_scope: "src/**".into(),
            secret_scope: String::new(),
            status: "pending".into(),
        };

        assert_eq!(
            agent_scope_violations(&root, &agent).unwrap(),
            vec!["README.md".to_string()]
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn agent_stale_paths_report_live_repo_changes() {
        let root = env::temp_dir().join(format!("devdrop-test-{}", now_nanos()));
        let repo = root.join("repo");
        let overlay = root.join("overlay");
        let base = agent_base_path(&root, "agent_test");
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::write(repo.join("src/app.rs"), "one\n").unwrap();
        copy_tree(&repo, &base).unwrap();
        copy_tree(&base, &overlay).unwrap();
        fs::write(repo.join("src/app.rs"), "user\n").unwrap();
        fs::write(overlay.join("src/app.rs"), "agent\n").unwrap();

        let agent = AgentRow {
            id: "agent_test".into(),
            repo_path: repo.to_string_lossy().into_owned(),
            overlay_path: overlay.to_string_lossy().into_owned(),
            write_scope: "src/**".into(),
            secret_scope: String::new(),
            status: "pending".into(),
        };

        assert_eq!(
            agent_stale_paths(&root, &agent).unwrap(),
            vec!["src/app.rs".to_string()]
        );
        fs::remove_dir_all(root).ok();
    }
}
