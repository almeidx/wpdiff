# wpdiff

Rust CLI tool that diffs WordPress plugins against their upstream versions from wordpress.org.

## Architecture

```
src/
  main.rs      CLI entry point, subcommand dispatch, upgrade logic
  plugin.rs    WP plugin header parsing, directory discovery
  source.rs    Source adapter trait, wordpress.org fetcher, version API
  diff.rs      Directory diff engine, file categorization, filtering
  output.rs    Terminal, JSON, unified diff, summary renderers
  progress.rs  Progress bar/spinner helpers with parallel suppression
```

## Key design decisions

- **Source adapter pattern**: `source::Source` trait allows adding new upstream sources (GitHub, GitLab) by implementing `fetch()`. Only wordpress.org is built in.
- **Category filtering**: files are categorized as source/artifact/asset/metadata. Default output hides artifacts and assets. Filtering happens post-diff via `DiffResult::apply()`.
- **Skipped directories**: `node_modules/`, `vendor/`, `external/`, `.git/`, `.svn/`, `.hg/` are pruned during directory walking (never traversed), but reported in output.
- **Upgrade flow**: everything happens in a temp staging dir. The live plugin is only replaced after user confirmation. Backup zip is created before swap.

## Building

```bash
cargo build                                                    # dev build
cargo build --release                                          # release build
cargo build --release --target x86_64-unknown-linux-musl       # linux static binary
```

## Testing

```bash
cargo test
```

## Conventions

- No comments that repeat what code does. Code should be self-documenting.
- No commented-out code.
- Prefer editing existing files over creating new ones.
- Error messages should be actionable — tell the user what to do, not just what went wrong.
- Progress bars are suppressed in parallel mode via `progress::suppress()`.
- All temp directories use `tempfile::TempDir` for automatic cleanup.

## Dependencies

| Crate | Purpose |
|---|---|
| clap | CLI argument parsing with derive |
| similar | Unified diff generation |
| colored | Terminal colors |
| reqwest | HTTP client (blocking, rustls) |
| zip | Zip archive reading/writing |
| serde/serde_json | JSON serialization |
| tempfile | Temp directories with auto-cleanup |
| walkdir | Recursive directory traversal |
| anyhow | Error handling |
| regex | WP plugin header parsing |
| log/env_logger | Logging |
| indicatif | Progress bars and spinners |
| terminal_size | Terminal width detection |
| glob-match | File path glob matching |
| mpatch | Patch application with fuzz matching |
| rayon | Parallel plugin processing |
