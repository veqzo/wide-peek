# widepeek

Windows-only Rust exe (`x86_64-pc-windows-msvc`). User docs: `README.md`, `docs/TROUBLESHOOTING.md`.

## Invariants

- **Always restore.** Every new code path that changes a display mode, a monitor device or a GPU setting also restores it: on normal exit, on error (drop guard), and through the state file for `RestoreAtLogon`. The README table "Restore guarantees" is the contract.
- **Hands off the game.** Watch Valorant only through the process list. widepeek opens no handle to the game process.
- **Least privilege.** Only the scheduled tasks and `install-*` / `uninstall-system` run elevated. Keep the launcher and the Riot Client unelevated.
- Each `unsafe` block gets a `// SAFETY:` comment.
- Log with `info!` / `warn!` from `src/log.rs`. User-facing text goes through `console` or `ui`.

## Checks

Run the CI steps before a PR; CI uses `-D warnings` and `--locked`:

```
cargo fmt --all --check
cargo clippy --target x86_64-pc-windows-msvc --all-targets --locked -- -D warnings
cargo test --target x86_64-pc-windows-msvc --locked
```

## Git

- `main` has a ruleset: changes land only through a PR with a green `build` check, branch up to date. Work on a branch.
- Release: a PR `chore: release x.y.z` bumps `version` in `Cargo.toml` (and `Cargo.lock`). After the merge, tag `main` with `vx.y.z`. CI fails when the tag and the crate version differ, and publishes the release from the tag.
- Write docs and README in Simplified Technical English, like the existing text.
