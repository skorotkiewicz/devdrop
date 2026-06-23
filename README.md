<p align="center">
  <img src="https://raw.githubusercontent.com/skorotkiewicz/devdrop/refs/heads/docs/logo.svg" width="50" height="50" alt="devdrop logo">
</p>

<h1 align="center">devdrop</h1>

<p align="center">Local-first workspace sync for developers.</p>

<p align="center">
  <a href="https://github.com/skorotkiewicz/devdrop/releases"><img alt="release" src="https://img.shields.io/github/v/release/skorotkiewicz/devdrop?style=flat-square"></a>
  <a href="https://aur.archlinux.org/packages/devdrop"><img alt="aur" src="https://img.shields.io/aur/version/devdrop?style=flat-square"></a>
  <img alt="license" src="https://img.shields.io/badge/license-MIT-176b5c?style=flat-square">
  <img alt="rust" src="https://img.shields.io/badge/rust-2024-7a4d12?style=flat-square">
</p>

devdrop keeps a code folder understandable across machines and agents: file
state, generated-folder ignores, Git status, encrypted secrets, history,
recovery, and reviewable agent overlays.

It is not a Git replacement. Git owns source history; devdrop owns the working
workspace around it.

## Install

Arch Linux:

```sh
yay -S devdrop
# or
paru -S devdrop
```

## Start

```sh
devdrop init ~/projects --remote ssh://server/backups/devdrop
cd ~/projects
devdrop sync
devdrop status
```

`devdrop init` creates `.devdrop/` metadata and `.devdrop.toml`. The remote is
stored once, so daily commands can stay short.

## Use

```sh
devdrop sync
devdrop ls ~/projects/api
devdrop ignored ~/projects/api
devdrop repo-status ~/projects/api
devdrop pin ~/projects/api/src
devdrop doctor ~/projects
```

## Config

`.devdrop.toml` stores the workspace defaults you do not want to pass every
time: remote URL, pins, and secret scope defaults.

```sh
devdrop remote add ssh://server/backups/devdrop
devdrop config set pins api/src,web/src
devdrop config set secrets.default.scope dev
devdrop remote ls
```

Example:

```toml
[remote]
url = "ssh://server/backups/devdrop"
auto_sync = true

pins = ["api/src", "web/src"]

[secrets.default]
scope = "dev"
```

## Sync

```sh
devdrop sync
```

`sync` is bidirectional when a remote is configured: it pulls remote changes,
indexes local changes, pushes updates, hydrates pinned files, and writes
conflict siblings when both sides changed.

Supported remotes:

```sh
devdrop init ~/projects --remote ssh://server/backups/devdrop
devdrop remote add /mnt/devdrop-remote
devdrop remote add file:///mnt/devdrop-remote
devdrop remote add ssh://host/path
```

Conflicts are written next to the original file:

```text
config.conflict-remote-123.ts
config.conflict-local-123.ts
```

Resolve one with:

```sh
devdrop conflicts ~/projects
devdrop conflicts resolve ~/projects/api/src/config.conflict-remote-123.ts --use conflict
```

## Secrets

For common key-value secrets:

```sh
devdrop secret set API_KEY=secret123
devdrop secret list
devdrop run --repo ~/projects/api -- printenv API_KEY
```

File-based secrets are still available:

```sh
devdrop secret add ~/projects/api/.env --scope dev
devdrop secret lock ~/projects/api/.env --scope dev
devdrop secret unlock ~/projects/api/.env --scope dev
```

If `DEVDROP_SECRET_KEY` is not set, devdrop creates a local workspace key in
`.devdrop/secret.key`.

## Edit And Agents

For the normal review flow:

```sh
cd ~/projects/api
devdrop edit
```

`edit` opens an overlay in `$EDITOR`, shows a diff when the editor exits, and
asks whether to accept the changes.

Lower-level agent commands are still available:

```sh
devdrop agent create --repo ~/projects/api --write-scope 'src/**' --secret-scope test
devdrop agent diff <agent-id>
devdrop agent accept <agent-id>
devdrop agent reject <agent-id>
```

Agent accepts are scoped: changes outside `--write-scope` are blocked. Stale
accepts are blocked if the real repo changed after the overlay was created.

## History And Recovery

```sh
devdrop history ~/projects/api/src/config.ts
devdrop recover ~/projects/api/src/config.ts
devdrop recover ~/projects/api/src/config.ts --hash fnv1a64:...
```

History and recovery are workspace safety tools, not source history. Commit
source changes with Git.

## Advanced

```sh
devdrop help more
devdrop daemon ~/projects --interval 5
devdrop workspace init ~/projects
devdrop remote init /mnt/devdrop-remote
```

The lower-level commands remain for scripts and compatibility, but the intended
daily path is `init --remote`, `sync`, `edit`, and `secret set`.
