# AGENTS.md

wpdiff is a single-binary Rust CLI for comparing locally installed WordPress
plugins with the official upstream packages from wordpress.org. It can show
diffs, summarize changes, export patches, list upstream versions, and upgrade
plugins while reapplying local customizations.

## The one rule that outranks everything

**Read-only commands must never mutate the WordPress installation, and upgrade
must never replace a live plugin until the backup, patch application, and user
confirmation paths have succeeded.** A failed download, fuzzy patch conflict,
filesystem error, or declined prompt must leave the installed plugin exactly as
it was.

Practical consequences:
- `diff`, `summary`, `export`, and `versions` are inspection commands. They may
  read the plugin tree and write only explicitly requested output files.
- `upgrade --dry-run` exercises staging and patch reapply without touching the
  live plugin directory.
- The live plugin swap is the last step of `upgrade`, after a backup zip exists
  and conflicts have either been resolved or surfaced for manual handling.
- Temporary staging must use `tempfile::TempDir` so failed runs clean up after
  themselves.

## Build / test / lint (CI runs exactly these)

    cargo test
    cargo clippy --all-targets -- -D warnings
    cargo fmt --check

All three must pass before a change is considered done. CI also builds the
static Linux release targets (`x86_64-unknown-linux-musl` and
`aarch64-unknown-linux-musl`).

## Conventions

- Error messages should be actionable: tell the user what path, plugin slug,
  version, or flag to fix.
- Keep filesystem mutations explicit and late. Prefer staging directories and
  atomic-ish swaps over in-place edits.
- Progress bars are suppressed during parallel work with `progress::suppress()`.
- Default diff output hides generated artifacts and binary assets; keep filtering
  changes additive and predictable.
- No comments that repeat what the code already says. Add comments only to
  explain non-obvious safety or WordPress/package-format behavior.

## Where things live

- `src/main.rs` — CLI definitions, subcommand dispatch, and top-level command
  flow.
- `src/plugin.rs` — WordPress plugin discovery and plugin-header parsing.
- `src/source.rs` — upstream source abstraction and wordpress.org downloader /
  version API.
- `src/diff.rs` — directory diffing, file categorization, filtering, and skip
  rules.
- `src/output.rs` — terminal, JSON, unified diff, and summary/table renderers.
- `src/upgrade.rs` — staging, backup, patch capture/reapply, conflict handling,
  and final plugin replacement.
- `src/progress.rs` — progress bar/spinner helpers and parallel suppression.
