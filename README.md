<p align="center">
  <img src="docs/logo.svg" width="50" height="50" alt="devdrop logo">
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
cargo build --release
./target/release/devdrop workspace init ~/code
cd ~/code
devdrop login
devdrop device enroll "MacBook Pro"
devdrop sync
devdrop status
```

## Use

```sh
devdrop ls ~/code/work
devdrop ignored ~/code/work/web
devdrop repo-status ~/code/work/api
devdrop hydrate ~/code/work/api/src/main.rs
devdrop pin ~/code/work/api
devdrop doctor ~/code
```

## Sync

```sh
devdrop remote init /mnt/devdrop-remote
devdrop sync ~/code --remote /mnt/devdrop-remote
devdrop sync ~/code --remote /mnt/devdrop-remote --pull
```

## Secrets

```sh
export DEVDROP_SECRET_KEY="local-passphrase"
devdrop secret add ~/code/work/api/.env --scope dev
devdrop secret lock ~/code/work/api/.env --scope dev
devdrop run --repo ~/code/work/api --secret-scope dev -- cargo test
```

## Agents

```sh
devdrop agent create --repo ~/code/work/api --write-scope 'src/**' --secret-scope test
devdrop overlay diff <agent-id>
devdrop overlay submit <agent-id>
devdrop agent accept <agent-id>
```

Agent accepts are scoped: changes outside `--write-scope` are blocked.

## Docs

Open [docs/index.html](docs/index.html) for the guided version.

The architecture plan lives in [PLAN.md](PLAN.md).
