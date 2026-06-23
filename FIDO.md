# FIDO

## Done

- [x] Workspace init/mount and local `.devdrop` storage
- [x] Login and local SQLite metadata database
- [x] Device enrollment and device listing
- [x] Device state sync between workspaces
- [x] Manual sync/pull with remote manifest
- [x] Remote namespace survives unhydrated workspace pushes
- [x] Local object store
- [x] Manual hydrate
- [x] Pin/unpin
- [x] Pinned paths hydrate automatically on pull
- [x] Dev-aware default ignore rules
- [x] `.devsyncignore` custom rules
- [x] Git repo detection and repo status
- [x] Stale branch warnings
- [x] Stale upstream blocks agent start
- [x] File history and recover
- [x] Encrypted `.env` add/lock/unlock/request/run
- [x] Secret sync as locked remote metadata
- [x] Agent overlay create/status/diff/submit/accept/reject
- [x] Agent write-scope enforcement
- [x] Stale agent accept protection
- [x] Pull conflict preserves local and remote edits
- [x] Remote delete conflict preserves local edits
- [x] Conflict listing
- [x] Conflict resolution for normal and delete conflicts
- [x] Tombstone sync for remote deletes
- [x] Doctor/status/ls/ignored workflow
- [x] End-to-end workflow smoke test

## Still Left

- [ ] Real cloud control plane: users, orgs, auth, workspace membership
- [ ] Real manifest service: versioned Merkle DAG and compare-and-swap commits
- [ ] Real blob service: chunking, resumable upload/download, encryption, garbage collection
- [ ] Proper device enrollment: keypairs, approval, key wrapping, trust levels
- [ ] Real daemon/watchers: inotify/FSEvents/Windows watcher instead of polling
- [ ] True lazy hydration/on-open VFS: FUSE/macFUSE/File Provider/WinFsp
- [ ] Strong secret architecture: workspace keys, grants, scoped agent access, audit logs
- [ ] Agent gateway: short-lived tokens, mounted remote workspace, command/secret policy
- [ ] Editor integration
- [ ] Shell integration
- [ ] Tray/menu-bar/status UI
- [ ] Ransomware/mass-delete protection and rollback
- [ ] Performance work for huge workspaces
- [ ] Proper cloud/API service layer and observability
