# wpdiff

Diff locally installed WordPress plugins against their official upstream
versions.

wpdiff is a single-binary CLI for finding local plugin customizations, exporting
patches, and upgrading plugins while preserving the changes you made on disk.
It compares your installed plugin tree with the matching package from
wordpress.org and keeps destructive work inside the explicit `upgrade` flow.

```sh
wpdiff akismet
wpdiff summary --all -C /var/www/html
wpdiff export akismet -o customizations.patch
wpdiff upgrade akismet --dry-run -C /var/www/html
```

## Install

Preferred:

```sh
brew install almeidx/tap/wpdiff
```

This installs the prebuilt GitHub Release artifact for Apple Silicon macOS or
Linux (`x86_64` / `aarch64`).

From crates.io:

```sh
cargo install wpdiff
```

Or download a release archive directly from
[GitHub Releases](https://github.com/almeidx/wpdiff/releases). Verify with the
published SHA-256 checksums.

## Quick start

Run from the WordPress root, or pass `-C` to point at it:

```sh
wpdiff akismet
wpdiff akismet -C /var/www/html
wpdiff /var/www/html/wp-content/plugins/akismet
```

By default wpdiff shows a colored unified diff of source and metadata changes
between the local plugin and the upstream version on wordpress.org.

## Usage

Summarize one plugin or every plugin in an installation:

```sh
wpdiff summary akismet
wpdiff summary --all -C /var/www/html
```

Export local customizations as a patch:

```sh
wpdiff export akismet
wpdiff export akismet -o my.patch
```

List upstream versions:

```sh
wpdiff versions akismet
wpdiff versions akismet --json
```

Upgrade with patch reapply:

```sh
wpdiff upgrade akismet -C /var/www/html
wpdiff upgrade akismet --to 5.5 -C /var/www/html
wpdiff upgrade akismet --dry-run -C /var/www/html
```

The upgrade command captures local customizations as a patch, downloads the
target version to a staging directory, reapplies the patch with fuzzy matching,
creates a backup zip of the current plugin, and swaps in the upgraded tree only
after confirmation. If hunks fail, wpdiff writes patch/reject files for manual
resolution.

## Filtering

Default output hides generated artifacts (`.min.js`, `vendor/`, etc.) and
binary assets (images, fonts). Include more when you need it:

```sh
wpdiff akismet --include-artifacts
wpdiff akismet --include-assets
wpdiff akismet --include-all
wpdiff akismet -x "assets/js/**"
wpdiff akismet -x assets -x templates
```

Directories such as `node_modules/`, `vendor/`, `external/`, `.git/`, `.svn/`,
and `.hg/` are always skipped during file walking.

## Output

```sh
wpdiff akismet
wpdiff akismet --json
wpdiff summary akismet --json
```

Terminal output is optimized for review. JSON output is intended for scripts and
automation.

## Development

Requires a stable Rust toolchain with Rust 2024 support (Rust 1.96 or newer).

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

CI runs the test suite and builds the release targets. Merging a release PR
publishes `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`,
and `aarch64-apple-darwin` binaries with SHA-256 checksums.

License: Apache-2.0.
