use super::config::load_config;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub fn workspace_root_for(path: &Path) -> Result<PathBuf, String> {
    require_dir(path)?;
    find_workspace_root(path).ok_or_else(|| {
        format!(
            "no workspace found at or above {}; run `devdrop init {}`",
            display_path(path),
            display_path(path)
        )
    })
}

pub fn copy_tree(src: &Path, dest: &Path) -> Result<(), String> {
    require_dir(src)?;
    fs::create_dir_all(dest).map_err(|err| format!("create {}: {err}", display_path(dest)))?;
    copy_tree_inner(src, dest, src)
}

pub fn sync_tree(src: &Path, dest: &Path) -> Result<(), String> {
    require_dir(src)?;
    fs::create_dir_all(dest).map_err(|err| format!("create {}: {err}", display_path(dest)))?;
    prune_missing(src, dest, dest)?;
    copy_tree(src, dest)
}

pub fn prune_missing(src_root: &Path, dest_root: &Path, current_dest: &Path) -> Result<(), String> {
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

pub fn copy_tree_inner(root: &Path, dest_root: &Path, current: &Path) -> Result<(), String> {
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

pub fn skip_overlay_component(rel: &Path) -> bool {
    rel.components().next().is_some_and(|part| {
        let part = part.as_os_str();
        part == ".git" || part == ".devdrop"
    })
}

pub fn require_dir(path: &Path) -> Result<(), String> {
    if path.is_dir() {
        Ok(())
    } else {
        Err(format!("not a directory: {}", display_path(path)))
    }
}

pub fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub fn rel_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

pub fn rel_or_dot(root: &Path, path: &Path) -> String {
    let rel = pin_path(root, path);
    if rel.is_empty() { ".".to_string() } else { rel }
}

pub fn find_workspace_root(path: &Path) -> Option<PathBuf> {
    let mut current = if path.is_dir() { path } else { path.parent()? };

    loop {
        if current.join(".devdrop").is_dir() {
            return Some(current.to_path_buf());
        }

        current = current.parent()?;
    }
}

pub fn pin_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

pub fn read_pins(root: &Path) -> Result<Vec<String>, String> {
    let path = root.join(".devdrop/pins");
    let mut pins = Vec::new();

    if path.exists() {
        let text = fs::read_to_string(&path).map_err(|err| format!("read pins: {err}"))?;
        pins.extend(
            text.lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(String::from),
        );
    }

    pins.extend(load_config(root)?.pins);
    pins.sort();
    pins.dedup();
    Ok(pins)
}

pub fn write_pins(path: &Path, pins: &[String]) -> Result<(), String> {
    let mut file = fs::File::create(path).map_err(|err| format!("write pins: {err}"))?;
    for pin in pins {
        writeln!(file, "{pin}").map_err(|err| format!("write pins: {err}"))?;
    }
    Ok(())
}
