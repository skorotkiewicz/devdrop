use crate::commands::{flag_value, required_arg};
use crate::db::{db_path, init_db, log_operation, query_lines, run_sql};
use crate::fs_util::{
    copy_tree, display_path, find_workspace_root, pin_path, require_dir, sync_tree,
};
use crate::git::{git_output, is_repo, repo_status, run_git};
use crate::index::tree_signature;
use crate::rules::{Action, Rule};
use crate::util::{hex_decode_string, json_string, now_nanos, now_secs, sql_string};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn cmd_agent(args: &[String]) -> Result<(), String> {
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

pub fn cmd_overlay(args: &[String]) -> Result<(), String> {
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
    let root = find_workspace_root(repo)
        .ok_or_else(|| "no workspace found; run `devdrop init <path>`".to_string())?;
    init_db(&root)?;
    ensure_agent_repo_fresh(&root, repo)?;
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

fn ensure_agent_repo_fresh(root: &Path, repo: &Path) -> Result<(), String> {
    if !is_repo(repo)
        || git_output(
            repo,
            &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
        )
        .is_none()
    {
        return Ok(());
    }

    run_git(repo, &["fetch", "--prune"])
        .map_err(|err| format!("refresh repo before creating agent: {err}"))?;
    let status = repo_status(repo);
    let behind = status.behind.unwrap_or(0);
    if behind == 0 {
        return Ok(());
    }

    let upstream = status.upstream.as_deref().unwrap_or("upstream");
    log_operation(
        root,
        "agent_create_stale",
        &pin_path(root, repo),
        &format!(
            "{{\"upstream\":{},\"behind\":{behind}}}",
            json_string(upstream)
        ),
        "blocked",
    )?;
    Err(format!(
        "{} is {behind} commits behind {upstream}; run `devdrop repo update {}` before creating an agent",
        display_path(repo),
        display_path(repo)
    ))
}

fn cmd_agent_status() -> Result<(), String> {
    let root = env::current_dir()
        .map_err(|err| format!("current dir: {err}"))
        .ok()
        .and_then(|dir| find_workspace_root(&dir))
        .ok_or_else(|| "no workspace found; run `devdrop init .`".to_string())?;
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

#[derive(Clone)]
pub struct AgentRow {
    pub id: String,
    pub repo_path: String,
    pub overlay_path: String,
    pub write_scope: String,
    pub secret_scope: String,
    pub status: String,
}

pub fn agent_overlay_path(root: &Path, id: &str) -> PathBuf {
    root.join(".devdrop/agents").join(id).join("overlay")
}

pub fn agent_base_path(root: &Path, id: &str) -> PathBuf {
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
    let root = find_workspace_root(&cwd)
        .ok_or_else(|| "no workspace found; run `devdrop init .`".to_string())?;
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
    let root = find_workspace_root(&cwd)
        .ok_or_else(|| "no workspace found; run `devdrop init .`".to_string())?;
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

pub fn agent_scope_violations(root: &Path, agent: &AgentRow) -> Result<Vec<String>, String> {
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

pub fn agent_stale_paths(root: &Path, agent: &AgentRow) -> Result<Vec<String>, String> {
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

pub fn write_scope_allows(scope: &str, rel: &str, is_dir: bool) -> bool {
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
