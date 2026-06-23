use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

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
        Some("status") => cmd_status(optional_path(args.get(1))?),
        Some("ls") => cmd_ls(optional_path(args.get(1))?),
        Some("ignored") => cmd_ignored(optional_path(args.get(1))?),
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
  devdrop status [path]
  devdrop ls [path]
  devdrop ignored [path]
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
            fs::create_dir_all(&path).map_err(|err| format!("create workspace: {err}"))?;
            fs::create_dir_all(path.join(".devdrop"))
                .map_err(|err| format!("create .devdrop: {err}"))?;
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(path.join(".devdrop/pins"))
                .map_err(|err| format!("create pins file: {err}"))?;
            println!("workspace initialized: {}", display_path(&path));
            Ok(())
        }
        _ => Err("usage: devdrop workspace init <path>".into()),
    }
}

fn cmd_status(root: PathBuf) -> Result<(), String> {
    require_dir(&root)?;

    let rules = Rules::load(&root)?;
    let counts = scan_workspace(&root, &rules)?;
    let pins = read_pins(&root).unwrap_or_default();

    println!("workspace: {}", display_path(&root));
    println!("entries: {}", counts.entries);
    println!("local: {}", counts.local);
    println!("ignored: {}", counts.ignored);
    println!("metadata-only: {}", counts.metadata_only);
    println!("local-only: {}", counts.local_only);
    println!("remote-only: {}", counts.remote_only);
    println!("secret-locked: {}", counts.secret_locked);
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

    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|err| format!("stat {}: {err}", display_path(&path)))?;
        let rel = rel_path(&root, &path);
        let action = rules.action_for(&rel, file_type.is_dir());
        let slash = if file_type.is_dir() { "/" } else { "" };
        let repo = if file_type.is_dir() && is_repo(&path) {
            " repo"
        } else {
            ""
        };

        println!(
            "{:<13} {}{}{}",
            action.state_label(),
            entry.file_name().to_string_lossy(),
            slash,
            repo
        );
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
        println!(
            "{} branch={} head={} dirty={} ahead={} behind={} upstream={}",
            display_path(&repo),
            status.branch.unwrap_or_else(|| "?".into()),
            status.head.unwrap_or_else(|| "?".into()),
            status.dirty,
            status
                .ahead
                .map(|count| count.to_string())
                .unwrap_or_else(|| "?".into()),
            status
                .behind
                .map(|count| count.to_string())
                .unwrap_or_else(|| "?".into()),
            status.upstream.unwrap_or_else(|| "none".into())
        );
    }

    Ok(())
}

fn cmd_doctor(root: PathBuf) -> Result<(), String> {
    require_dir(&root)?;
    let rules = Rules::load(&root)?;

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
    println!("daemon: not implemented");
    println!("remote manifest: not configured");
    println!("secret vault: not implemented");
    Ok(())
}

fn cmd_hydrate(path: PathBuf) -> Result<(), String> {
    if path.exists() {
        println!("already local: {}", display_path(&path));
        Ok(())
    } else {
        Err(format!(
            "no remote manifest configured; cannot hydrate {}",
            display_path(&path)
        ))
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
        println!("pinned: {rel}");
    } else {
        pins.retain(|pin| pin != &rel);
        write_pins(&pins_path, &pins)?;
        println!("unpinned: {rel}");
    }

    Ok(())
}

fn optional_path(arg: Option<&String>) -> Result<PathBuf, String> {
    arg.map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(|| env::current_dir().map_err(|err| format!("current dir: {err}")))
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
            .filter(|rule| rule.matches(rel, is_dir))
            .map(|rule| rule.action)
            .last()
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
    repos: usize,
    dirty_repos: usize,
}

fn scan_workspace(root: &Path, rules: &Rules) -> Result<Counts, String> {
    let mut counts = Counts::default();

    walk_dirs(root, root, rules, &mut |path, file_type, action| {
        counts.entries += 1;
        match action {
            Action::Sync => counts.local += 1,
            Action::Ignore => counts.ignored += 1,
            Action::MetadataOnly => counts.metadata_only += 1,
            Action::LocalOnly => counts.local_only += 1,
            Action::HydrateOnAccess => counts.remote_only += 1,
            Action::Secret => counts.secret_locked += 1,
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
        branch: git_output(path, &["rev-parse", "--abbrev-ref", "HEAD"]),
        head: git_output(path, &["rev-parse", "--short", "HEAD"]),
        upstream,
        ahead: Some(ahead),
        behind: Some(behind),
        dirty: repo_dirty(path),
    }
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
}
