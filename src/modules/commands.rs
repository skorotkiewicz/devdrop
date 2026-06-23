use super::agent::{cmd_agent, cmd_overlay};
use super::config::{cmd_config_get, cmd_config_set, cmd_remote_add, cmd_remote_ls};
use super::git::{cmd_repo, cmd_repo_status};
use super::remote::{cmd_conflicts, cmd_remote};
use super::secrets::{cmd_run, cmd_secret, cmd_secret_list, cmd_secret_set};
use super::sync::{cmd_daemon, cmd_sync};
use super::workspace::{
    cmd_device, cmd_doctor, cmd_edit, cmd_history, cmd_hydrate, cmd_ignored, cmd_init, cmd_login,
    cmd_ls, cmd_pin, cmd_recover, cmd_status, cmd_workspace,
};
use std::env;
use std::path::PathBuf;

pub fn run(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("help") if args.get(1).map(String::as_str) == Some("more") => {
            print_more_help();
            Ok(())
        }
        None | Some("help") | Some("-h") | Some("--help") => {
            print_help();
            Ok(())
        }
        Some("init") => cmd_init(&args[1..]),
        Some("login") => cmd_login(args.get(1)),
        Some("workspace") => cmd_workspace(&args[1..]),
        Some("device") => cmd_device(&args[1..]),
        Some("repo") => cmd_repo(&args[1..]),
        Some("remote") => match args.get(1).map(String::as_str) {
            Some("add") => cmd_remote_add(&args[1..]),
            Some("ls") => cmd_remote_ls(&args[1..]),
            _ => cmd_remote(&args[1..]),
        },
        Some("secret") => match args.get(1).map(String::as_str) {
            Some("set") => cmd_secret_set(&args[1..]),
            Some("list") => cmd_secret_list(&args[1..]),
            _ => cmd_secret(&args[1..]),
        },
        Some("edit") => cmd_edit(optional_path(args.get(1))?),
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
        Some("config") => match args.get(1).map(String::as_str) {
            Some("get") => cmd_config_get(&args[1..]),
            Some("set") => cmd_config_set(&args[1..]),
            _ => Err("usage: devdrop config get|set <key> [value]".into()),
        },
        Some(other) => Err(format!("unknown command `{other}`; run `devdrop help`")),
    }
}

pub fn print_help() {
    println!(
        "\
devdrop - local-first workspace helper

Usage:
  devdrop init [path] [--remote <url>]
  devdrop sync
  devdrop status
  devdrop config get|set <key> [value]
  devdrop remote add <url> | ls
  devdrop secret set <key>=<value> [--scope <scope>]
  devdrop secret list
  devdrop edit [path]
  devdrop help more"
    );
}

pub fn print_more_help() {
    println!(
        "\
devdrop - advanced commands

Usage:
  devdrop init [path] [--remote <url>]
  devdrop login [user]
  devdrop workspace init|mount <path>
  devdrop device enroll <name>
  devdrop device list
  devdrop repo update [path]
  devdrop remote init <path> | add <url> | ls
  devdrop secret add <path> --scope <scope>
  devdrop secret set <key>=<value> [--scope <scope>]
  devdrop secret list
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
  devdrop daemon [path] [--remote <url>] [--interval <seconds>] [--once]
  devdrop sync
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
  devdrop doctor [path]
  devdrop config get|set <key> [value]
  devdrop edit [path]"
    );
}

pub fn optional_path(arg: Option<&String>) -> Result<PathBuf, String> {
    arg.map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(|| env::current_dir().map_err(|err| format!("current dir: {err}")))
}

pub fn first_positional(args: &[String]) -> Option<&String> {
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

pub fn required_path(arg: Option<&String>, command: &str) -> Result<PathBuf, String> {
    arg.map(PathBuf::from)
        .ok_or_else(|| format!("usage: devdrop {command} <path>"))
}

pub fn required_arg<'a>(arg: Option<&'a String>, command: &str) -> Result<&'a str, String> {
    arg.map(String::as_str)
        .ok_or_else(|| format!("usage: devdrop {command} <id>"))
}

pub fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|window| window[0] == flag)
        .map(|window| window[1].as_str())
}
