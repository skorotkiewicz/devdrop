use super::commands::{first_positional, flag_value, optional_path};
use super::fs_util::{display_path, workspace_root_for};
use super::index::{IndexSnapshot, carry_indexed_remote_nodes, collect_index, write_index};
use super::remote::{
    finish_remote, prepare_remote, pull_remote, push_remote, read_remote_config, write_remote_url,
};
use super::rules::Rules;
use super::util::fnv_bytes;
use std::path::Path;
use std::thread;
use std::time::Duration;

pub fn cmd_daemon(args: &[String]) -> Result<(), String> {
    let root = optional_path(first_positional(args))?;
    let root = workspace_root_for(&root)?;
    let remote = flag_value(args, "--remote")
        .map(str::to_string)
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
            .map(|url| format!(" remote={url}"))
            .unwrap_or_default()
    );

    loop {
        let rules = Rules::load(&root)?;
        let snapshot = collect_index(&root, &rules)?;
        let signature = snapshot_signature(&snapshot);

        if last_signature != Some(signature) {
            write_index(&root, &rules, &snapshot)?;
            if let Some(remote) = &remote {
                let handle = prepare_remote(&root, remote)?;
                push_remote(&root, &handle.path, &snapshot)?;
                finish_remote(&handle)?;
                write_remote_url(&root, remote)?;
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

        thread::sleep(Duration::from_secs(interval));
    }
}

pub fn cmd_sync(args: &[String]) -> Result<(), String> {
    let root = optional_path(first_positional(args))?;
    let root = workspace_root_for(&root)?;
    let remote = flag_value(args, "--remote")
        .map(str::to_string)
        .or_else(|| read_remote_config(&root).ok().flatten());

    if args.iter().any(|arg| arg == "--pull") {
        let remote = remote.ok_or_else(|| "sync --pull requires --remote <url>".to_string())?;
        let handle = prepare_remote(&root, &remote)?;
        pull_remote(&root, &handle.path)?;
        write_remote_url(&root, &remote)?;
        println!("pulled remote manifest: {}", remote);
        return Ok(());
    }

    if let Some(remote) = remote {
        let handle = prepare_remote(&root, &remote)?;
        pull_remote(&root, &handle.path)?;
        let snapshot = sync_local_index(&root)?;
        push_remote(&root, &handle.path, &snapshot)?;
        finish_remote(&handle)?;
        write_remote_url(&root, &remote)?;
        println!("synced remote: {}", handle.url);
        println!("nodes: {}", snapshot.nodes.len());
        println!("blobs: {}", snapshot.blobs.len());
        println!("repos: {}", snapshot.repos.len());
        return Ok(());
    }

    let snapshot = sync_local_index(&root)?;
    println!("synced local index: {}", display_path(&root));
    println!("nodes: {}", snapshot.nodes.len());
    println!("blobs: {}", snapshot.blobs.len());
    println!("repos: {}", snapshot.repos.len());
    Ok(())
}

pub fn sync_local_index(root: &Path) -> Result<IndexSnapshot, String> {
    let rules = Rules::load(root)?;
    let mut snapshot = collect_index(root, &rules)?;
    carry_indexed_remote_nodes(root, &mut snapshot)?;
    write_index(root, &rules, &snapshot)?;
    Ok(snapshot)
}

pub fn snapshot_signature(snapshot: &IndexSnapshot) -> u64 {
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
