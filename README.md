# wpdiff

Diff locally installed WordPress plugins against their official upstream versions. Find what changed, export patches, and upgrade plugins while preserving your customizations.

## Install

Download the latest binary from [releases](https://github.com/cyph/wpdiff/releases), or build from source:

```bash
cargo install --path .
```

## Usage

### Diff a plugin

```bash
wpdiff akismet                          # from within a WordPress directory
wpdiff akismet -C /var/www/html         # specify WordPress root
wpdiff /path/to/wp-content/plugins/akismet  # explicit path
```

Shows a colored unified diff of local changes vs the upstream version on wordpress.org.

### Summary

```bash
wpdiff summary akismet                  # git-style diffstat for one plugin
wpdiff summary --all -C /var/www/html   # table of all modified plugins
```

### Export a patch

```bash
wpdiff export akismet                   # writes akismet-5.3.7.patch
wpdiff export akismet -o my.patch       # custom output path
```

### List available versions

```bash
wpdiff versions akismet                 # shows recent versions with installed marker
wpdiff versions akismet --json          # machine-readable output
```

### Upgrade with patch reapply

```bash
wpdiff upgrade akismet -C /var/www/html           # upgrade to latest, reapply customizations
wpdiff upgrade akismet --to 5.5 -C /var/www/html  # upgrade to specific version
wpdiff upgrade akismet --dry-run -C /var/www/html  # test without modifying anything
```

The upgrade command:
1. Captures your local customizations as a patch
2. Downloads the target version to a staging area
3. Applies the patch using fuzzy matching
4. Creates a zip backup of the current plugin
5. Swaps in the upgraded version only after you confirm

If some patch hunks fail, the tool saves a `.patch` file and `.rej` file for manual resolution.

## Filtering

By default, only source code and metadata changes are shown. Build artifacts (`.min.js`, `vendor/`, etc.) and binary assets (images, fonts) are hidden.

```bash
wpdiff akismet --include-artifacts      # also show build artifacts
wpdiff akismet --include-assets         # also show binary assets
wpdiff akismet --include-all            # show everything
wpdiff akismet -x "assets/js/**"        # exclude specific paths
wpdiff akismet -x assets -x templates   # exclude multiple paths
```

Directories like `node_modules/`, `vendor/`, `external/`, `.git/` are always skipped during file walking.

## All plugins

Scan every plugin in a WordPress installation:

```bash
wpdiff --all -C /var/www/html           # diff all plugins
wpdiff summary --all -C /var/www/html   # summary table of all plugins
```

Plugins not found on wordpress.org are reported as unmatched.

## Output formats

```bash
wpdiff akismet                          # colored terminal output (default)
wpdiff akismet --json                   # JSON output
wpdiff summary akismet --json           # JSON summary
```

## Building

Requires Rust 1.70+.

```bash
cargo build --release
```

Cross-compile for Linux (static musl binary):

```bash
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
```

## License

MIT
