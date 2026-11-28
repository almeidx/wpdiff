use anyhow::{Context, Result, bail};
use colored::Colorize;
use log::{debug, info, warn};
use rustix::fs::{CWD, RenameFlags, renameat_with};
use rustix::io::Errno;
use similar::{ChangeTag, TextDiff};
use std::collections::BTreeMap;
use std::fmt::{self, Write as _};
use std::fs;
use std::io::{self, IsTerminal, Write as _};
use std::path::Path;
use tempfile::TempDir;
use walkdir::WalkDir;

use crate::diff::{self, CONTEXT_RADIUS, DirDiff, FileDiff, Filter, SkippedDirs};
use crate::output;
use crate::plugin::PluginMeta;
use crate::source::WpOrg;

pub struct Options<'a> {
    /// Version to upgrade to; the latest release when `None`.
    pub target_version: Option<&'a str>,
    /// Replace the plugin without asking, and without interactive review.
    pub yes: bool,
    pub dry_run: bool,
    pub strict_whitespace: bool,
}

/// How the customizations patch fared against the staged new version.
#[derive(Default)]
struct Reapplied {
    exact_files: usize,
    /// Fully applied files with at least one inexact hunk, and their worst match score.
    fuzzy_files: Vec<(String, f64)>,
    failed_files: usize,
    rejects: String,
}

#[derive(Clone, Copy)]
enum Verdict {
    Clean,
    Fuzzy,
    Failed,
}

impl Reapplied {
    const fn verdict(&self) -> Verdict {
        if self.failed_files > 0 {
            Verdict::Failed
        } else if self.fuzzy_files.is_empty() {
            Verdict::Clean
        } else {
            Verdict::Fuzzy
        }
    }
}

pub fn run(meta: &PluginMeta, opts: &Options) -> Result<()> {
    info!(
        "Upgrading {} v{} → {}",
        meta.name,
        meta.version,
        opts.target_version.unwrap_or("latest")
    );

    // The backup, patch and review files go to the working directory, which
    // therefore must not be part of the directory the upgrade replaces.
    let cwd = std::env::current_dir()?.canonicalize()?;
    if cwd.starts_with(&meta.dir) {
        bail!(
            "Cannot upgrade from inside {}: the backup and patch files are written to the \
             working directory. Run wpdiff from outside the plugin directory.",
            meta.dir.display()
        );
    }

    info!("Capturing local customizations...");
    let wporg = WpOrg::new()?;
    let current_upstream = wporg.download(&meta.slug, &meta.version)?;
    // The swap replaces the whole directory, dependency directories included,
    // so every local change is collected: the ones the patch cannot carry are
    // at least reported.
    let (customizations, left_behind) =
        diff::diff_directories_in_full(&meta.dir, current_upstream.path(), opts.strict_whitespace)?
            .partition(&Filter::default());
    let has_customizations = !customizations.files.is_empty();
    print_customizations(&customizations);
    print_left_behind(left_behind);

    let new_version = if let Some(version) = opts.target_version {
        version.to_string()
    } else {
        info!("Checking latest version...");
        wporg.plugin_info(&meta.slug)?.latest_version
    };

    if new_version == meta.version {
        println!(
            "\n  {} Already on the latest version (v{}).",
            "✓".green().bold(),
            meta.version
        );
        return Ok(());
    }

    info!("Downloading v{new_version}...");
    let new_upstream = wporg.download(&meta.slug, &new_version)?;

    println!(
        "\n  Upgrading: {} → {}",
        format!("v{}", meta.version).red(),
        format!("v{new_version}").green(),
    );

    let staging_dir = TempDir::new().context("Failed to create staging directory")?;
    let staged_plugin = staging_dir.path().join(&meta.slug);
    copy_dir_recursive(new_upstream.path(), &staged_plugin)?;

    let patch_text = customizations.patch();
    let reapplied = if has_customizations {
        info!("Applying customizations to new version...");
        let reapplied = reapply(&patch_text, &staged_plugin)?;
        print_reapply_summary(&reapplied);
        reapplied
    } else {
        Reapplied::default()
    };
    let verdict = reapplied.verdict();

    let artifact = |suffix: &str| format!("{}-{}{suffix}", meta.slug, meta.version);
    let save = |path: &str, content: &str| {
        fs::write(path, content).with_context(|| format!("Failed to write {path}"))
    };

    if !reapplied.fuzzy_files.is_empty() {
        let interactive = !opts.yes && !opts.dry_run && io::stdin().is_terminal();
        let review = review_fuzzy_files(
            &reapplied.fuzzy_files,
            new_upstream.path(),
            &staged_plugin,
            &new_version,
            interactive,
        )?;
        if !review.is_empty() {
            let review_path = artifact("-review.diff");
            save(&review_path, &review)?;
            println!("  Saved review diff: {}", review_path.bold());
        }
    }

    if opts.dry_run {
        print_dry_run_verdict(has_customizations, verdict, &meta.version, &new_version);
        return Ok(());
    }

    if matches!(verdict, Verdict::Failed) {
        let patch_path = artifact("-customizations.patch");
        save(&patch_path, &patch_text)?;
        println!("  Saved patch: {}", patch_path.bold());

        if !reapplied.rejects.is_empty() {
            let rej_path = artifact(".rej");
            save(&rej_path, &reapplied.rejects)?;
            println!("  Saved rejected hunks: {}", rej_path.bold());
        }
    }

    if !opts.yes {
        let question = match verdict {
            Verdict::Failed => "  Replace live plugin? (some customizations failed to apply)",
            Verdict::Fuzzy => {
                "  Replace live plugin? (some hunks matched inexactly - review recommended)"
            }
            Verdict::Clean => "  Replace live plugin with upgraded version?",
        };
        if !confirm(question)? {
            println!("  Aborted. The installed plugin was not changed.");
            return Ok(());
        }
    }

    let backup_path = artifact("-backup.zip");
    info!("Backing up current plugin to {backup_path}...");
    create_zip_backup(&meta.dir, &backup_path)?;

    info!("Replacing plugin directory...");
    replace_plugin_dir(&meta.dir, &staged_plugin)?;

    println!(
        "\n  {} {} upgraded to v{}",
        "✓".green().bold(),
        meta.name,
        new_version,
    );
    println!("  Backup: {backup_path}");

    Ok(())
}

fn print_customizations(customizations: &DirDiff) {
    if customizations.files.is_empty() {
        println!("  No local customizations found.");
        return;
    }

    let (insertions, deletions) = customizations.line_changes();
    println!(
        "  Found {} customized files ({} insertions, {} deletions)",
        customizations.files.len().to_string().bold(),
        insertions.to_string().green(),
        deletions.to_string().red(),
    );
    for file in &customizations.files {
        println!("    {} {}", "~".yellow(), file.path);
    }
}

/// Groups changes by top-level directory, with `""` for the plugin root.
fn by_outer_folder(files: Vec<FileDiff>) -> BTreeMap<String, DirDiff> {
    let mut groups: BTreeMap<String, Vec<FileDiff>> = BTreeMap::new();
    for file in files {
        let folder = file.path.split_once('/').map_or("", |(outer, _)| outer);
        groups.entry(folder.to_string()).or_default().push(file);
    }
    groups
        .into_iter()
        .map(|(folder, files)| (folder, DirDiff::new(files, SkippedDirs::default(), 0)))
        .collect()
}

fn print_left_behind(files: Vec<FileDiff>) {
    if files.is_empty() {
        return;
    }

    println!(
        "\n  {} {} changed files are build artifacts, assets or dependencies and are {}.",
        "⚠".yellow().bold(),
        files.len().to_string().bold(),
        "not carried over".yellow(),
    );
    println!("    The upgrade replaces them with upstream's; only the backup zip keeps yours.");

    let groups: Vec<(String, DirDiff)> = by_outer_folder(files)
        .into_iter()
        .map(|(folder, changes)| {
            let label = if folder.is_empty() {
                "(plugin root)".to_string()
            } else {
                format!("{folder}/")
            };
            (label, changes)
        })
        .collect();
    let width = groups
        .iter()
        .map(|(label, _)| label.len())
        .max()
        .unwrap_or(0);

    for (label, changes) in &groups {
        let (insertions, deletions) = changes.line_changes();
        println!(
            "    {label:<width$}  {} added, {} changed, {} removed  ({} {})",
            changes.summary.added,
            changes.summary.modified,
            changes.summary.removed,
            format!("+{insertions}").green(),
            format!("-{deletions}").red(),
        );
    }
}

fn reapply(patch_text: &str, staged_plugin: &Path) -> Result<Reapplied> {
    let patches = mpatch::parse_patches(patch_text)
        .map_err(|e| anyhow::anyhow!("Failed to parse patch: {e:?}"))?;
    let options = mpatch::ApplyOptions::builder().fuzz_factor(0.6).build();
    let batch = mpatch::apply_patches_to_dir(&patches, staged_plugin, options);

    let mut reapplied = Reapplied::default();

    for (file_path, result) in &batch.results {
        let file = file_path.to_string_lossy();

        let report = match result {
            Ok(patch_result) => &patch_result.report,
            Err(e) => {
                reapplied.failed_files += 1;
                println!("    {} {} - {:?}", "✗".red().bold(), file, e);
                continue;
            }
        };

        let mut worst_score = None;
        for status in &report.hunk_results {
            if let mpatch::HunkApplyStatus::Applied {
                match_type: mpatch::MatchType::Fuzzy { score },
                ..
            } = status
            {
                worst_score = Some(worst_score.map_or(*score, |worst: f64| worst.min(*score)));
                println!(
                    "    {} {} - hunk applied with fuzz ({:.0}% match, may be incorrect)",
                    "⚠".yellow().bold(),
                    file,
                    score * 100.0,
                );
            }
        }

        let failures = report.failures();
        if !failures.is_empty() {
            reapplied.failed_files += 1;
            println!(
                "    {} {} - {} failed hunks",
                "✗".red().bold(),
                file,
                failures.len(),
            );
            for failure in &failures {
                let _ = write!(
                    reapplied.rejects,
                    "--- Failed hunk #{} in {} ---\n{:?}\n\n",
                    failure.hunk_index, file, failure.reason,
                );
            }
        } else if let Some(score) = worst_score {
            reapplied.fuzzy_files.push((file.into_owned(), score));
        } else {
            reapplied.exact_files += 1;
            debug!("  Applied cleanly: {file}");
        }
    }

    Ok(reapplied)
}

fn print_reapply_summary(reapplied: &Reapplied) {
    let applied = reapplied.exact_files + reapplied.fuzzy_files.len();
    let fuzzy = reapplied.fuzzy_files.len().to_string().yellow();

    println!();
    match reapplied.verdict() {
        Verdict::Clean => println!(
            "  {} All customizations applied cleanly ({applied} files)",
            "✓".green().bold(),
        ),
        Verdict::Fuzzy => {
            println!(
                "  {} {applied} files applied, but {fuzzy} with fuzzy matching (review recommended)",
                "⚠".yellow().bold(),
            );
            println!("    Fuzzy matches may have applied your changes in the wrong location or");
            println!("    overwritten upstream changes. Review the diffs below.");
        }
        Verdict::Failed => {
            println!(
                "  {} {applied}/{} files applied, {} failed",
                "⚠".yellow().bold(),
                applied + reapplied.failed_files,
                reapplied.failed_files.to_string().red(),
            );
            if !reapplied.fuzzy_files.is_empty() {
                println!(
                    "    Additionally, {fuzzy} files applied with fuzzy matching (review recommended)",
                );
            }
        }
    }
}

/// Shows each fuzzy-matched file against the new upstream; when interactive,
/// rewrites the staged file with the sections the user chose. Returns the
/// diffs as shown, before any resolution.
fn review_fuzzy_files(
    fuzzy_files: &[(String, f64)],
    upstream_dir: &Path,
    staged_plugin: &Path,
    new_version: &str,
    interactive: bool,
) -> Result<String> {
    if interactive {
        println!(
            "\n  {} {} files with fuzzy matches need review:\n",
            "Resolve".yellow().bold(),
            fuzzy_files.len(),
        );
    } else {
        println!(
            "\n  {} Fuzzy-matched changes (non-interactive, keeping patched versions):\n",
            "Review".yellow().bold(),
        );
    }

    let read = |path: &Path| {
        fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))
    };
    let mut review = String::new();

    for (rel_path, score) in fuzzy_files {
        let patched_file = staged_plugin.join(rel_path);
        let upstream_text = read(&upstream_dir.join(rel_path))?;
        let patched_text = read(&patched_file)?;
        let percent = score * 100.0;

        let unified = diff::unified_diff(
            &upstream_text,
            &patched_text,
            &format!("a/{rel_path} (upstream v{new_version})"),
            &format!("b/{rel_path} (patched, {percent:.0}% fuzz match)"),
        );
        if unified.is_empty() {
            continue;
        }
        review.push_str(&unified);

        if interactive {
            let resolved = resolve_file_hunks(
                rel_path,
                percent,
                &upstream_text,
                &patched_text,
                |section| {
                    inquire::Select::new(
                        &format!("  Section {section}:"),
                        vec![Resolution::Ours, Resolution::Upstream, Resolution::Both],
                    )
                    .prompt()
                    .context("Review cancelled; the installed plugin was left unchanged")
                },
            )?;
            fs::write(&patched_file, resolved)
                .with_context(|| format!("Failed to write resolved {rel_path}"))?;
        } else {
            println!("  {} {rel_path} ({percent:.0}% match)", "diff".bold());
            output::write_colored_diff(&mut io::stdout().lock(), &unified, "  ")?;
        }
    }

    Ok(review)
}

fn print_dry_run_verdict(
    has_customizations: bool,
    verdict: Verdict,
    current_version: &str,
    new_version: &str,
) {
    println!();
    if !has_customizations {
        println!(
            "  {} Dry run: upgrade v{current_version} → v{new_version} would apply cleanly (no customizations).",
            "✓".green().bold(),
        );
        return;
    }
    match verdict {
        Verdict::Clean => println!(
            "  {} Dry run: all customizations would reapply cleanly after upgrade to v{new_version}.",
            "✓".green().bold(),
        ),
        Verdict::Fuzzy => {
            println!(
                "  {} Dry run: customizations would apply but {} with fuzzy matching.",
                "⚠".yellow().bold(),
                "some hunks matched inexactly".yellow(),
            );
            println!("    These files should be reviewed after upgrading to confirm correctness.");
        }
        Verdict::Failed => println!(
            "  {} Dry run: some customizations would fail to reapply after upgrade to v{new_version}.",
            "⚠".yellow().bold(),
        ),
    }
}

fn confirm(question: &str) -> Result<bool> {
    print!("{question} [y/N] ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(answer.trim().eq_ignore_ascii_case("y"))
}

enum Resolution {
    Ours,
    Upstream,
    Both,
}

impl fmt::Display for Resolution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Ours => "Keep our change (green lines)",
            Self::Upstream => "Use upstream only (discard green, keep red)",
            Self::Both => "Keep both (upstream + our additions)",
        })
    }
}

fn resolve_file_hunks(
    rel_path: &str,
    match_percent: f64,
    upstream_text: &str,
    patched_text: &str,
    mut choose: impl FnMut(usize) -> Result<Resolution>,
) -> Result<String> {
    let diff = TextDiff::from_lines(upstream_text, patched_text);
    let groups = diff.grouped_ops(CONTEXT_RADIUS);

    let eol_of = |text: &str| if text.contains("\r\n") { "\r\n" } else { "\n" };
    let upstream_eol = eol_of(upstream_text);
    let patched_eol = eol_of(patched_text);
    let upstream_lines: Vec<&str> = diff.iter_old_slices().collect();
    let patched_lines: Vec<&str> = diff.iter_new_slices().collect();
    let mut output = String::with_capacity(upstream_text.len().max(patched_text.len()));
    let mut glue = "";
    // Only the last line of a file can lack a terminator; `glue` supplies one when
    // such a line ends up followed by another.
    let mut push = |lines: &[&str], eol: &'static str| {
        for line in lines {
            output.push_str(glue);
            output.push_str(line);
            glue = if line.ends_with(['\n', '\r']) {
                ""
            } else {
                eol
            };
        }
    };
    let mut last_upstream_idx = 0;

    println!(
        "  {} {} ({:.0}% match) — {} sections to review",
        "file".bold(),
        rel_path,
        match_percent,
        groups.len(),
    );

    for (group_idx, group) in groups.iter().enumerate() {
        let (Some(first), Some(last)) = (group.first(), group.last()) else {
            continue;
        };
        let upstream_range = first.old_range().start..last.old_range().end;
        let patched_range = first.new_range().start..last.new_range().end;

        push(
            &upstream_lines[last_upstream_idx..upstream_range.start],
            upstream_eol,
        );
        last_upstream_idx = upstream_range.end;

        println!(
            "\n    {} Section {}/{} (lines {}-{}):",
            "─".repeat(3).dimmed(),
            group_idx + 1,
            groups.len(),
            upstream_range.start + 1,
            upstream_range.end,
        );

        for change in group.iter().flat_map(|op| diff.iter_changes(op)) {
            let line = change.to_string_lossy();
            match change.tag() {
                ChangeTag::Delete => print!("      {}", format!("-{line}").red()),
                ChangeTag::Insert => print!("      {}", format!("+{line}").green()),
                ChangeTag::Equal => print!("      {}", format!(" {line}").dimmed()),
            }
            if !line.ends_with('\n') {
                println!();
            }
        }

        match choose(group_idx + 1)? {
            Resolution::Ours => {
                push(&patched_lines[patched_range], patched_eol);
                println!("      {} Keeping our change", "→".green());
            }
            Resolution::Upstream => {
                push(&upstream_lines[upstream_range], upstream_eol);
                println!("      {} Using upstream version", "→".yellow());
            }
            Resolution::Both => {
                push(&upstream_lines[upstream_range], upstream_eol);
                for op in group
                    .iter()
                    .filter(|op| op.tag() != similar::DiffTag::Equal)
                {
                    push(&patched_lines[op.new_range()], patched_eol);
                }
                println!("      {} Keeping both versions", "→".cyan());
            }
        }
    }

    push(&upstream_lines[last_upstream_idx..], upstream_eol);

    println!("    {} {} resolved\n", "✓".green().bold(), rel_path);
    Ok(output)
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    for entry in WalkDir::new(src) {
        let entry = entry?;
        let target = dst.join(entry.path().strip_prefix(src)?);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)
                .with_context(|| format!("Failed to create {}", target.display()))?;
        } else {
            fs::copy(entry.path(), &target)
                .with_context(|| format!("Failed to write {}", target.display()))?;
        }
    }
    Ok(())
}

/// Puts a copy of `staged` in place of the live plugin directory.
///
/// The copy is made next to the plugin first, so the live directory changes
/// in a single exchange: `WordPress` sees the old plugin or the new one, never a
/// missing or half-written directory, and a failed copy leaves the old one
/// untouched.
fn replace_plugin_dir(live: &Path, staged: &Path) -> Result<()> {
    let (Some(plugins_dir), Some(name)) = (live.parent(), live.file_name()) else {
        bail!(
            "Cannot replace {}: it has no parent directory",
            live.display()
        );
    };
    // A sibling shares the plugin's filesystem, which a rename needs, and
    // WordPress ignores dot-prefixed entries of the plugins directory.
    let sibling = tempfile::Builder::new()
        .prefix(&format!(".{}.wpdiff-", name.to_string_lossy()))
        .tempdir_in(plugins_dir)
        .with_context(|| {
            format!(
                "Failed to create a staging directory in {}",
                plugins_dir.display()
            )
        })?;
    let incoming = sibling.path().join("new");
    let outgoing = sibling.path().join("old");
    copy_dir_recursive(staged, &incoming)?;

    if let Err(e) = swap_dirs(&incoming, live, &outgoing) {
        if !live.exists() {
            // Removing the staging directory now would delete the plugin.
            let kept = sibling.keep();
            bail!(
                "Failed to swap the upgraded plugin into {}: {e}. The previous plugin is in {}; \
                 move it back by hand.",
                live.display(),
                kept.join("old").display()
            );
        }
        return Err(e).with_context(|| {
            format!("Failed to swap the upgraded plugin into {}", live.display())
        });
    }

    let previous = sibling.path().to_path_buf();
    if let Err(e) = sibling.close() {
        warn!(
            "The previous plugin files could not be removed from {}: {e}",
            previous.display()
        );
    }
    Ok(())
}

/// Moves `incoming` to `live`. What was at `live` ends up at `incoming` or
/// at `outgoing`.
fn swap_dirs(incoming: &Path, live: &Path, outgoing: &Path) -> io::Result<()> {
    match renameat_with(CWD, incoming, CWD, live, RenameFlags::EXCHANGE) {
        // NFS and kernels before 3.15 cannot exchange two paths.
        Err(Errno::INVAL | Errno::NOSYS | Errno::NOTSUP) => {
            swap_by_renames(incoming, live, outgoing)
        }
        result => Ok(result?),
    }
}

/// Not atomic: between the two renames nothing is at `live`.
fn swap_by_renames(incoming: &Path, live: &Path, outgoing: &Path) -> io::Result<()> {
    fs::rename(live, outgoing)?;
    fs::rename(incoming, live).or_else(|e| fs::rename(outgoing, live).and(Err(e)))
}

fn create_zip_backup(dir: &Path, zip_path: &str) -> Result<()> {
    let file =
        fs::File::create(zip_path).with_context(|| format!("Failed to create {zip_path}"))?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    for entry in WalkDir::new(dir).min_depth(1) {
        let entry = entry?;
        let rel = entry.path().strip_prefix(dir)?.to_string_lossy();

        if entry.file_type().is_dir() {
            zip.add_directory(format!("{rel}/"), options)?;
        } else {
            zip.start_file(rel, options)?;
            let mut f = fs::File::open(entry.path())?;
            io::copy(&mut f, &mut zip)?;
        }
    }

    zip.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const UPSTREAM: &str = "function first() {}\nfunction second() {}\nfunction third() {}\nfunction fourth() {}\nfunction fifth() {}\n";
    const CUSTOMIZED: &str = "function first() {}\nfunction second() {}\nfunction third() { custom(); }\nfunction fourth() {}\nfunction fifth() {}\n";

    fn staged(content: &str) -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.php"), content).unwrap();
        dir
    }

    fn customization() -> String {
        diff::unified_diff(UPSTREAM, CUSTOMIZED, "a/a.php", "b/a.php")
    }

    #[test]
    fn reapply_carries_a_customization_onto_unchanged_upstream() {
        let dir = staged(UPSTREAM);
        let reapplied = reapply(&customization(), dir.path()).unwrap();

        assert!(matches!(reapplied.verdict(), Verdict::Clean));
        assert_eq!(reapplied.exact_files, 1);
        assert_eq!(
            fs::read_to_string(dir.path().join("a.php")).unwrap(),
            CUSTOMIZED
        );
    }

    #[test]
    fn reapply_reports_hunks_that_no_longer_fit() {
        let rewritten = "alpha\nbeta\ngamma\ndelta\nepsilon\n";
        let dir = staged(rewritten);
        let reapplied = reapply(&customization(), dir.path()).unwrap();

        assert!(matches!(reapplied.verdict(), Verdict::Failed));
        assert_eq!(reapplied.failed_files, 1);
        assert!(
            reapplied
                .rejects
                .starts_with("--- Failed hunk #1 in a.php ---")
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("a.php")).unwrap(),
            rewritten
        );
    }

    #[test]
    fn reapply_flags_customizations_that_only_fit_inexactly() {
        let drifted = UPSTREAM.replace("function second() {}", "function second($arg) {}");
        let dir = staged(&drifted);
        let reapplied = reapply(&customization(), dir.path()).unwrap();

        assert!(matches!(reapplied.verdict(), Verdict::Fuzzy));
        assert_eq!(reapplied.exact_files, 0);
        assert_eq!(reapplied.fuzzy_files.len(), 1);
        assert_eq!(reapplied.fuzzy_files[0].0, "a.php");
        assert_eq!(
            fs::read_to_string(dir.path().join("a.php")).unwrap(),
            CUSTOMIZED.replace("function second() {}", "function second($arg) {}")
        );
    }

    #[test]
    fn reapply_keeps_final_newline_and_line_endings() {
        for (upstream, local) in [
            ("one\ntwo\nthree\n", "one\ntwo\nTHREE\n"),
            ("one\r\ntwo\r\nthree\r\n", "one\r\ntwo\r\nTHREE\r\n"),
        ] {
            let (local_dir, upstream_dir) = (staged(local), staged(upstream));
            let patch = diff::diff_directories(local_dir.path(), upstream_dir.path(), false)
                .unwrap()
                .patch();

            let reapplied = reapply(&patch, upstream_dir.path()).unwrap();

            assert!(matches!(reapplied.verdict(), Verdict::Clean));
            assert_eq!(
                fs::read_to_string(upstream_dir.path().join("a.php")).unwrap(),
                local
            );
        }
    }

    #[test]
    fn copy_reproduces_a_nested_tree_in_a_new_directory() {
        let src = tempfile::tempdir().unwrap();
        fs::create_dir_all(src.path().join("inc/deep")).unwrap();
        fs::create_dir_all(src.path().join("empty")).unwrap();
        fs::write(src.path().join("main.php"), "<?php\n").unwrap();
        fs::write(src.path().join("inc/deep/x.php"), "deep\n").unwrap();
        let out = tempfile::tempdir().unwrap();
        let dst = out.path().join("not/yet/there");

        copy_dir_recursive(src.path(), &dst).unwrap();

        assert_eq!(fs::read_to_string(dst.join("main.php")).unwrap(), "<?php\n");
        assert_eq!(
            fs::read_to_string(dst.join("inc/deep/x.php")).unwrap(),
            "deep\n"
        );
        assert!(dst.join("empty").is_dir());
    }

    #[test]
    fn left_behind_changes_are_totalled_per_outer_folder() {
        use crate::diff::{FileCategory, FileStatus};
        let file = |path: &str, status, insertions, deletions| FileDiff {
            path: path.to_string(),
            status,
            category: FileCategory::Artifact,
            insertions,
            deletions,
            diff_text: String::new(),
        };

        let groups = by_outer_folder(vec![
            file("app.min.js", FileStatus::Removed, 0, 4),
            file("vendor/a.php", FileStatus::Added, 2, 0),
            file("vendor/lib/deep/b.php", FileStatus::Modified, 3, 1),
            file("dist/out.js", FileStatus::Added, 5, 0),
        ]);

        let totals: Vec<_> = groups
            .iter()
            .map(|(folder, changes)| {
                let summary = &changes.summary;
                (
                    folder.as_str(),
                    (summary.added, summary.modified, summary.removed),
                    changes.line_changes(),
                )
            })
            .collect();
        assert_eq!(
            totals,
            [
                ("", (0, 0, 1), (0, 4)),
                ("dist", (1, 0, 0), (5, 0)),
                ("vendor", (1, 1, 0), (5, 1)),
            ]
        );
    }

    fn dir_with(files: &[(&str, &str)]) -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, content) in files {
            let path = dir.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
        dir
    }

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn replacing_the_plugin_swaps_in_the_staged_tree_and_leaves_nothing_behind() {
        let plugins = dir_with(&[("akismet/old.php", "old\n"), ("akismet/inc/x.php", "x\n")]);
        let staged = dir_with(&[("new.php", "new\n"), ("views/v.php", "v\n")]);
        let live = plugins.path().join("akismet");

        replace_plugin_dir(&live, staged.path()).unwrap();

        assert_eq!(entries(plugins.path()), ["akismet"]);
        assert_eq!(entries(&live), ["new.php", "views"]);
        assert_eq!(fs::read_to_string(live.join("views/v.php")).unwrap(), "v\n");
    }

    #[test]
    fn a_failed_copy_leaves_the_live_plugin_untouched() {
        let plugins = dir_with(&[("akismet/old.php", "old\n")]);
        let live = plugins.path().join("akismet");
        let missing = plugins.path().join("no-such-staging-dir");

        assert!(replace_plugin_dir(&live, &missing).is_err());

        assert_eq!(entries(plugins.path()), ["akismet"]);
        assert_eq!(fs::read_to_string(live.join("old.php")).unwrap(), "old\n");
    }

    #[test]
    fn swapping_by_renames_keeps_the_previous_tree_aside() {
        let root = dir_with(&[("live/old.php", "old\n"), ("incoming/new.php", "new\n")]);
        let at = |name: &str| root.path().join(name);

        swap_by_renames(&at("incoming"), &at("live"), &at("outgoing")).unwrap();

        assert_eq!(entries(root.path()), ["live", "outgoing"]);
        assert_eq!(entries(&at("live")), ["new.php"]);
        assert_eq!(entries(&at("outgoing")), ["old.php"]);
    }

    #[test]
    fn swapping_moves_the_previous_tree_out_of_the_way() {
        let root = dir_with(&[("live/old.php", "old\n"), ("incoming/new.php", "new\n")]);
        let at = |name: &str| root.path().join(name);

        swap_dirs(&at("incoming"), &at("live"), &at("outgoing")).unwrap();

        assert_eq!(entries(&at("live")), ["new.php"]);
        // An exchange leaves the old tree at `incoming`, the fallback at `outgoing`.
        let previous = entries(root.path()).into_iter().find(|name| name != "live");
        assert_eq!(entries(&at(&previous.unwrap())), ["old.php"]);
    }

    #[test]
    fn swapping_by_renames_restores_the_plugin_when_the_new_tree_is_missing() {
        let root = dir_with(&[("live/old.php", "old\n")]);
        let at = |name: &str| root.path().join(name);

        assert!(swap_by_renames(&at("incoming"), &at("live"), &at("outgoing")).is_err());

        assert_eq!(entries(root.path()), ["live"]);
        assert_eq!(entries(&at("live")), ["old.php"]);
    }

    #[test]
    fn backup_zip_holds_every_file_of_the_plugin() {
        let plugin = tempfile::tempdir().unwrap();
        fs::create_dir_all(plugin.path().join("inc/deep")).unwrap();
        fs::write(plugin.path().join("main.php"), "<?php\n").unwrap();
        fs::write(plugin.path().join("inc/deep/x.php"), "deep\n").unwrap();
        let out = tempfile::tempdir().unwrap();
        let zip_path = out.path().join("backup.zip");

        create_zip_backup(plugin.path(), zip_path.to_str().unwrap()).unwrap();

        let mut archive = zip::ZipArchive::new(fs::File::open(&zip_path).unwrap()).unwrap();
        let mut content = String::new();
        io::Read::read_to_string(
            &mut archive.by_name("inc/deep/x.php").unwrap(),
            &mut content,
        )
        .unwrap();
        assert_eq!(content, "deep\n");
        assert!(archive.by_name("main.php").is_ok());
    }

    #[test]
    fn resolving_sections_keeps_crlf_line_endings() {
        let fixture = |second: &str, twelfth: &str| -> String {
            (1..=14)
                .map(|number| match number {
                    2 => format!("{second}\r\n"),
                    12 => format!("{twelfth}\r\n"),
                    _ => format!("line {number}\r\n"),
                })
                .collect()
        };
        let upstream = fixture("upstream 2", "upstream 12");
        let patched = fixture("patched 2", "patched 12");
        let resolve = |choices: [Resolution; 2]| {
            let mut choices = choices.into_iter();
            let choose = |_: usize| Ok(choices.next().unwrap());
            resolve_file_hunks("main.php", 90.0, &upstream, &patched, choose).unwrap()
        };

        assert_eq!(resolve([Resolution::Ours, Resolution::Ours]), patched);
        assert_eq!(
            resolve([Resolution::Upstream, Resolution::Upstream]),
            upstream
        );
        assert_eq!(
            resolve([Resolution::Upstream, Resolution::Ours]),
            fixture("upstream 2", "patched 12"),
        );
    }
}
