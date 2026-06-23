use super::fs_util::{display_path, find_workspace_root};
use super::util::now_nanos;
use std::fs;
use std::path::Path;
use std::process::Command;

pub fn openssl_crypt(decrypt: bool, input: &Path, output: &Path) -> Result<(), String> {
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
    ]);
    command.arg("-pass").arg(secret_pass_arg(input, output)?);
    if decrypt {
        command.arg("-d");
    }
    command.arg("-in").arg(input).arg("-out").arg(output);
    run_command(command, "openssl").map(|_| ())
}

pub fn openssl_decrypt_to_string(input: &Path) -> Result<String, String> {
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
        "-d",
    ]);
    command.arg("-pass").arg(secret_pass_arg(input, input)?);
    command.arg("-in").arg(input);
    let bytes = run_command(command, "openssl")?;
    String::from_utf8(bytes).map_err(|err| format!("secret is not utf-8: {err}"))
}

pub fn secret_pass_arg(input: &Path, output: &Path) -> Result<String, String> {
    if std::env::var("DEVDROP_SECRET_KEY").is_ok() {
        return Ok("env:DEVDROP_SECRET_KEY".to_string());
    }

    let root = find_workspace_root(output)
        .or_else(|| find_workspace_root(input))
        .ok_or_else(|| "no workspace found; run `devdrop init <path>`".to_string())?;
    let path = root.join(".devdrop/secret.key");
    if !path.exists() {
        let key = format!("devdrop-secret-{}", now_nanos());
        fs::write(&path, key).map_err(|err| format!("write {}: {err}", display_path(&path)))?;
    }
    Ok(format!("file:{}", path.to_string_lossy()))
}

pub fn run_command(mut command: Command, name: &str) -> Result<Vec<u8>, String> {
    let output = command
        .output()
        .map_err(|err| format!("run {name}: {err}"))?;

    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

pub fn command_ok(command: &str, args: &[&str]) -> bool {
    Command::new(command)
        .args(args)
        .output()
        .is_ok_and(|output| output.status.success())
}
