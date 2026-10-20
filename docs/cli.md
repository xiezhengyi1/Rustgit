# CLI guide

Build the engine with `cargo build --release -p rustgit-cli --features iroh`. The executable is
`target/release/rgit` (`rgit.exe` on Windows). Run `rgit --help` or `rgit <command> --help` for all options.

## Local versions

```bash
rgit init
rgit config user.name "Ann"
rgit status
rgit commit -m "First version"
rgit commit -m "Selected files" src/ README.md
rgit log
rgit diff
rgit diff HEAD~1 HEAD
rgit branch feature
rgit switch feature
rgit merge feature
rgit restore --from HEAD~3 path/to/file
rgit restore --from HEAD~3 .
rgit revert <commit>
rgit shelve -m "Work in progress"
rgit unshelve 1
rgit gc
rgit verify
rgit reindex
rgit stats
```

Switching refuses to overwrite uncommitted work. Restore preserves history and unrelated local files.
A revert creates a new commit. Use `rgit resolve` after editing merge conflicts.

## Host, invite, and join

Connections require device authentication and encryption by default. Device identities use macOS
Keychain or Windows Credential Manager. Invitation links contain enrollment secrets: share them privately.

```bash
# Hosting computer
rgit serve --init /srv/project.rgit --listen 0.0.0.0:7878
# In a second terminal on that computer
rgit -C /srv/project.rgit invite create --role write --expires 24h --hint tcp://192.168.1.5:7878

# Other computer: replace the placeholder with the actual invitation
rgit join 'projectapp://join/...' myproject
cd myproject
rgit commit -m "My changes"
rgit push

# Another enrolled working copy
rgit pull
```

`serve --init` creates an empty hosted repository. Publish a first commit from an enrolled working
copy before other devices clone it. The desktop app can host and publish an existing project for you.
`fetch` updates remote-tracking references without changing working files; `pull` fetches and merges.
Rejected pushes require fetching and merging before sending again. Force updates and deletions are
disabled unless explicitly enabled by the host and permitted by the device role.

```bash
rgit -C /srv/project.rgit devices list
rgit -C /srv/project.rgit devices role <fingerprint-prefix> read
rgit -C /srv/project.rgit devices revoke <fingerprint-prefix>
rgit -C /srv/project.rgit invite list
rgit -C /srv/project.rgit invite revoke <id>
rgit device id
```

Revocation prevents future access; it cannot remove data already fetched by another device.

See [protocol and security](protocol.md), [SSH](ssh.md), [LAN discovery](discovery.md),
and [Iroh and relays](relay.md) for advanced setup and limitations.
