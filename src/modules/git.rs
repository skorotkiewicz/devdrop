use super::commands::{first_positional, optional_path};
use super::fs_util::{display_path, require_dir};
use super::index::walk_dirs;
use super::rules::Rules;
use super::util::{json_optional, json_string};
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn cmd_repo(args: &[String]) -> Result<(), String> {
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

pub fn cmd_repo_status(args: &[String]) -> Result<(), String> {
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

pub fn find_repos(root: &Path, rules: &Rules) -> Result<Vec<PathBuf>, String> {
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

pub fn is_repo(path: &Path) -> bool {
    path.join(".git").exists()
}

pub fn repo_dirty(path: &Path) -> bool {
    git_output(path, &["status", "--porcelain"]).is_some_and(|text| !text.trim().is_empty())
}

pub struct RepoStatus {
    pub remote_url: Option<String>,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub upstream: Option<String>,
    pub ahead: Option<usize>,
    pub behind: Option<usize>,
    pub dirty: bool,
}

pub fn repo_status(path: &Path) -> RepoStatus {
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

pub fn stale_repo_warning(path: &Path, status: &RepoStatus) -> Option<String> {
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

pub fn print_repo_status_json(path: &Path, status: &RepoStatus) {
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

pub fn run_git(path: &Path, args: &[&str]) -> Result<(), String> {
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

pub fn git_output(path: &Path, args: &[&str]) -> Option<String> {
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
