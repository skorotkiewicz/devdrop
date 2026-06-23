use crate::agent::{
    AgentRow, agent_base_path, agent_scope_violations, agent_stale_paths, write_scope_allows,
};
use crate::fs_util::copy_tree;
use crate::git::{RepoStatus, stale_repo_warning};
use crate::index::is_conflict_path;
use crate::remote::conflict_base_path;
use crate::rules::{Action, Rule, Rules, default_rules, wildcard_match};
use crate::secrets::parse_env;
use crate::util::{now_nanos, sql_string};
use std::path::{Path, PathBuf};
use std::{env, fs};

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
