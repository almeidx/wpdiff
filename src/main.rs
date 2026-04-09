mod diff;
mod output;
mod plugin;
mod progress;
mod source;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use colored::Colorize;
use diff::FileCategory;
use log::{LevelFilter, debug, info, warn};
use std::collections::HashSet;
use std::fs;
use std::io::{self, Write};
use std::path::Path;

#[derive(Parser)]
#[command(
    name = "wpdiff",
    about = "Diff locally installed WordPress plugins against their upstream versions",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    #[command(flatten)]
    common: CommonArgs,

    #[command(flatten)]
    filter: FilterArgs,
}

#[derive(Subcommand)]
enum Command {
    /// Show a summary of changes (no diffs)
    Summary {
        #[command(flatten)]
        common: CommonArgs,
        #[command(flatten)]
        filter: FilterArgs,
    },
    /// Export changes as a .patch file
    Export {
        /// Output file path (default: <slug>-<version>.patch)
        #[arg(short, long)]
        output: Option<String>,
        #[command(flatten)]
        common: CommonArgs,
        #[command(flatten)]
        filter: FilterArgs,
    },
    /// List available versions from wordpress.org
    Versions {
        /// Plugin slug or path
        plugin: Option<String>,
        /// `WordPress` root directory
        #[arg(short = 'C', long = "dir")]
        dir: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Upgrade plugin and attempt to reapply local customizations
    Upgrade {
        /// Plugin slug or path
        plugin: Option<String>,
        /// `WordPress` root directory
        #[arg(short = 'C', long = "dir")]
        dir: Option<String>,
        /// Target version (default: latest)
        #[arg(long)]
        to: Option<String>,
        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
        /// Test if upgrade + patch would succeed without modifying anything
        #[arg(long)]
        dry_run: bool,
        /// Include whitespace and line ending changes
        #[arg(long)]
        whitespace: bool,
    },
}

#[derive(Parser, Clone)]
struct CommonArgs {
    /// Plugin slug or path to plugin directory
    plugin: Option<String>,

    /// Scan all plugins in a `WordPress` installation
    #[arg(long)]
    all: bool,

    /// `WordPress` root directory
    #[arg(short = 'C', long = "dir")]
    dir: Option<String>,

    /// Include whitespace and line ending changes
    #[arg(long)]
    whitespace: bool,

    /// Increase verbosity (-v info, -vv debug, -vvv trace)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    /// Suppress all non-error output
    #[arg(short, long, conflicts_with = "verbose", global = true)]
    quiet: bool,
}

impl CommonArgs {
    fn base_dir(&self) -> Option<&Path> {
        self.dir.as_deref().map(Path::new)
    }

    fn require_plugin(&self) -> Result<&str> {
        self.plugin
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Provide a plugin slug or path, or use --all"))
    }
}

#[derive(Parser, Clone)]
struct FilterArgs {
    /// Include build artifacts (.min.js, vendor/, etc.)
    #[arg(long)]
    include_artifacts: bool,

    /// Include binary assets (images, fonts)
    #[arg(long)]
    include_assets: bool,

    /// Include all file categories (artifacts + assets)
    #[arg(long)]
    include_all: bool,

    /// Exclude files matching glob patterns (repeatable)
    #[arg(short = 'x', long = "exclude", value_name = "GLOB")]
    exclude: Vec<String>,

    /// Output as JSON instead of terminal format
    #[arg(long)]
    json: bool,
}

impl FilterArgs {
    fn included_categories(&self) -> HashSet<FileCategory> {
        let mut cats = HashSet::new();
        cats.insert(FileCategory::Source);
        cats.insert(FileCategory::Metadata);

        if self.include_all || self.include_artifacts {
            cats.insert(FileCategory::Artifact);
        }
        if self.include_all || self.include_assets {
            cats.insert(FileCategory::Asset);
        }

        cats
    }

    fn apply_filters(&self, result: &diff::DiffResult) -> diff::DiffResult {
        result.apply(&self.included_categories(), &self.exclude)
    }
}

fn main() {
    let cli = Cli::parse();

    let (verbose, quiet) = match &cli.command {
        Some(Command::Summary { common, .. } | Command::Export { common, .. }) => {
            (common.verbose, common.quiet)
        }
        Some(Command::Versions { .. } | Command::Upgrade { .. }) => {
            (cli.common.verbose, cli.common.quiet)
        }
        None => (cli.common.verbose, cli.common.quiet),
    };

    init_logger(verbose, quiet);

    if let Err(e) = run(cli) {
        log::error!("{e:#}");
        std::process::exit(1);
    }
}

fn init_logger(verbose: u8, quiet: bool) {
    let level = if quiet {
        LevelFilter::Error
    } else {
        match verbose {
            0 => LevelFilter::Warn,
            1 => LevelFilter::Info,
            2 => LevelFilter::Debug,
            _ => LevelFilter::Trace,
        }
    };

    env_logger::Builder::new()
        .filter_level(level)
        .format_timestamp(None)
        .format_target(false)
        .init();
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        None => {
            if cli.common.plugin.is_none() && !cli.common.all {
                Cli::parse_from(["wpdiff", "--help"]);
                return Ok(());
            }
            render_command(&cli.common, &cli.filter, &RenderMode::Diff)
        }
        Some(Command::Summary { common, filter }) => {
            render_command(&common, &filter, &RenderMode::Summary)
        }
        Some(Command::Export {
            output,
            common,
            filter,
        }) => cmd_export(&common, &filter, output.as_deref()),
        Some(Command::Versions { plugin, dir, json }) => {
            cmd_versions(plugin.as_deref(), dir.as_deref().map(Path::new), json)
        }
        Some(Command::Upgrade {
            plugin,
            dir,
            to,
            yes,
            dry_run,
            whitespace,
        }) => cmd_upgrade(
            plugin.as_deref(),
            dir.as_deref().map(Path::new),
            to.as_deref(),
            yes,
            dry_run,
            whitespace,
        ),
    }
}

enum RenderMode {
    Diff,
    Summary,
}

fn render_output(
    result: &diff::DiffResult,
    mode: &RenderMode,
    json: bool,
    is_all: bool,
) -> Result<()> {
    let mut out = std::io::stdout().lock();
    if json {
        output::render_json(result, &mut out)?;
    } else {
        match (mode, is_all) {
            (RenderMode::Diff, _) => output::render_terminal(result, &mut out)?,
            (RenderMode::Summary, true) => output::render_summary_compact(result, &mut out)?,
            (RenderMode::Summary, false) => output::render_summary(result, &mut out)?,
        }
    }
    Ok(())
}

fn render_command(common: &CommonArgs, filter: &FilterArgs, mode: &RenderMode) -> Result<()> {
    if common.all {
        let is_summary = matches!(mode, RenderMode::Summary);
        return run_all(common, filter, is_summary, |filtered| {
            render_output(filtered, mode, filter.json, true)
        });
    }

    let result = resolve_and_diff(common)?;
    let filtered = filter.apply_filters(&result);
    render_output(&filtered, mode, filter.json, false)
}

fn cmd_versions(plugin_arg: Option<&str>, base_dir: Option<&Path>, json: bool) -> Result<()> {
    let plugin_arg = plugin_arg.ok_or_else(|| anyhow::anyhow!("Provide a plugin slug"))?;

    let slug = resolve_slug(plugin_arg, base_dir);

    let installed_version = plugin::resolve_plugin_path(plugin_arg, base_dir)
        .ok()
        .and_then(|p| plugin::discover_plugin(&p).ok())
        .map(|m| m.version);

    info!("Fetching versions for {slug}...");
    let info = source::fetch_plugin_versions(&slug)?;

    if json {
        let json = serde_json::json!({
            "slug": slug,
            "name": info.name,
            "latest": info.latest_version,
            "installed": installed_version,
            "versions": info.versions,
        });
        println!("{}", serde_json::to_string_pretty(&json)?);
        return Ok(());
    }

    println!("{} {}", info.name.bold(), format!("({slug})").dimmed());

    if let Some(ref installed) = installed_version {
        println!("  Installed: {}", format!("v{installed}").yellow().bold());
    }
    println!(
        "  Latest:    {}",
        format!("v{}", info.latest_version).green().bold()
    );

    println!("\n  {}", "Available versions:".dimmed());

    let display_count = 20;
    let total = info.versions.len();
    let start = total.saturating_sub(display_count);

    if start > 0 {
        println!("  {} ({} older versions hidden)", "...".dimmed(), start);
    }

    for version in &info.versions[start..] {
        let is_installed = installed_version.as_deref() == Some(version.as_str());
        let is_latest = version == &info.latest_version;

        let marker = if is_installed && is_latest {
            format!("{}", "◀ installed, latest".green())
        } else if is_installed {
            format!("{}", "◀ installed".yellow())
        } else if is_latest {
            format!("{}", "◀ latest".green())
        } else {
            String::new()
        };

        let ver_display = if is_installed {
            format!("v{version}").yellow().bold().to_string()
        } else if is_latest {
            format!("v{version}").green().to_string()
        } else {
            format!("v{version}")
        };

        println!("    {ver_display} {marker}");
    }

    println!("\n  {total} versions total");

    Ok(())
}

fn resolve_slug(plugin_arg: &str, base_dir: Option<&Path>) -> String {
    plugin::resolve_plugin_path(plugin_arg, base_dir).map_or_else(
        |_| {
            plugin_arg
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or(plugin_arg)
                .to_string()
        },
        |path| {
            path.file_name().map_or_else(
                || plugin_arg.to_string(),
                |n| n.to_string_lossy().to_string(),
            )
        },
    )
}

fn cmd_export(common: &CommonArgs, filter: &FilterArgs, output_path: Option<&str>) -> Result<()> {
    let result = resolve_and_diff(common)?;
    let filtered = filter.apply_filters(&result);

    if filtered.files.is_empty() {
        bail!("No differences to export");
    }

    let path = match output_path {
        Some(p) => p.to_string(),
        None => format!("{}-{}.patch", filtered.plugin_slug, filtered.plugin_version),
    };

    let file =
        fs::File::create(&path).with_context(|| format!("Failed to create patch file {path}"))?;
    let mut writer = std::io::BufWriter::new(file);
    output::render_unified(&filtered, &mut writer)?;

    info!("Patch exported to {path}");
    Ok(())
}

fn resolve_and_diff(common: &CommonArgs) -> Result<diff::DiffResult> {
    let plugin_arg = common.require_plugin()?;

    debug!("Resolving plugin: {plugin_arg}");
    let plugin_path = plugin::resolve_plugin_path(plugin_arg, common.base_dir())?;
    let mut result = diff_single_plugin(&plugin_path, common.whitespace)?;
    result.latest_version = source::fetch_plugin_versions(&result.plugin_slug)
        .ok()
        .map(|info| info.latest_version);
    Ok(result)
}

fn diff_single_plugin(plugin_path: &Path, strict_whitespace: bool) -> Result<diff::DiffResult> {
    let registry = source::Registry::new();
    diff_plugin_with_registry(&registry, plugin_path, strict_whitespace)
}

fn diff_plugin_with_registry(
    registry: &source::Registry,
    plugin_path: &Path,
    strict_whitespace: bool,
) -> Result<diff::DiffResult> {
    let meta = plugin::discover_plugin(plugin_path)?;

    info!(
        "Comparing {} v{} against upstream...",
        meta.name, meta.version
    );
    debug!("Plugin directory: {}", meta.dir.display());
    debug!("Main file: {}", meta.main_file.display());

    let upstream = registry.fetch(&meta, None)?;

    debug!("Upstream extracted to: {}", upstream.path.display());

    diff::diff_directories(
        &meta.dir,
        &upstream.path,
        &meta.slug,
        &meta.version,
        strict_whitespace,
    )
}

fn run_all(
    common: &CommonArgs,
    filter: &FilterArgs,
    is_summary: bool,
    render: impl Fn(&diff::DiffResult) -> anyhow::Result<()>,
) -> Result<()> {
    use rayon::prelude::*;

    let base = common.dir.as_deref().unwrap_or(".");
    debug!("Scanning all plugins in {base}");

    let discovered = plugin::discover_all(Path::new(base))?;

    let plugins: Vec<plugin::PluginMeta> = discovered
        .into_iter()
        .filter_map(|r| match r {
            Ok(meta) => Some(meta),
            Err(e) => {
                warn!("{e:#}");
                None
            }
        })
        .collect();

    debug!("Found {} plugins", plugins.len());

    let whitespace = common.whitespace;
    let registry = source::Registry::new();

    let overall = progress::bar(
        plugins.len() as u64,
        "  {spinner:.green} Scanning [{bar:30.cyan/dim}] {pos}/{len} plugins",
    );
    progress::suppress(true);

    let results: Vec<(String, Result<diff::DiffResult>)> = plugins
        .par_iter()
        .map(|meta| {
            let result = diff_plugin_with_registry(&registry, &meta.dir, whitespace);
            overall.inc(1);
            (meta.slug.clone(), result)
        })
        .collect();

    progress::suppress(false);
    overall.finish_and_clear();

    let version_pb = progress::bar(
        results.len() as u64,
        "  {spinner:.green} Checking versions [{bar:30.cyan/dim}] {pos}/{len}",
    );
    progress::suppress(true);

    let results: Vec<(String, Result<diff::DiffResult>)> = results
        .into_par_iter()
        .map(|(slug, result)| {
            let result = result.map(|mut r| {
                r.latest_version = source::fetch_plugin_versions(&slug)
                    .ok()
                    .map(|info| info.latest_version);
                r
            });
            version_pb.inc(1);
            (slug, result)
        })
        .collect();

    progress::suppress(false);
    version_pb.finish_and_clear();

    let mut sorted: Vec<(String, Result<diff::DiffResult>)> = results;
    sorted.sort_by(|(a, _), (b, _)| a.to_lowercase().cmp(&b.to_lowercase()));

    let mut diffs = Vec::new();
    let mut clean_count = 0usize;
    let mut unmatched = Vec::new();

    for (slug, result) in sorted {
        match result {
            Ok(diff_result) => {
                let filtered = filter.apply_filters(&diff_result);
                if filtered.files.is_empty() {
                    clean_count += 1;
                    debug!("{slug}: no differences");
                } else {
                    diffs.push(filtered);
                }
            }
            Err(e) => {
                unmatched.push((slug, format!("{e:#}")));
            }
        }
    }

    if is_summary && !filter.json {
        output::render_summary_table(
            &diffs,
            clean_count,
            unmatched.len(),
            &mut std::io::stdout().lock(),
        )?;
    } else if diffs.is_empty() && unmatched.is_empty() {
        info!("All plugins match their upstream versions.");
        return Ok(());
    } else {
        for diff in &diffs {
            render(diff)?;
        }
        if clean_count > 0 || !unmatched.is_empty() {
            let mut out = std::io::stdout().lock();
            writeln!(out)?;
            writeln!(
                out,
                "{} changed, {} unchanged, {} unmatched",
                diffs.len(),
                clean_count,
                unmatched.len()
            )?;
        }
    }

    if !unmatched.is_empty() {
        for (slug, reason) in &unmatched {
            warn!("{slug} - {reason}");
        }
    }

    Ok(())
}

fn cmd_upgrade(
    plugin_arg: Option<&str>,
    base_dir: Option<&Path>,
    target_version: Option<&str>,
    yes: bool,
    dry_run: bool,
    whitespace: bool,
) -> Result<()> {
    let plugin_arg =
        plugin_arg.ok_or_else(|| anyhow::anyhow!("Provide a plugin slug or path to upgrade"))?;

    let plugin_path = plugin::resolve_plugin_path(plugin_arg, base_dir)?;
    let meta = plugin::discover_plugin(&plugin_path)?;

    info!(
        "Upgrading {} v{} → {}",
        meta.name,
        meta.version,
        target_version.unwrap_or("latest")
    );

    info!("Capturing local customizations...");
    let registry = source::Registry::new();
    let current_upstream = registry.fetch(&meta, None)?;

    let diff_result = diff::diff_directories(
        &meta.dir,
        &current_upstream.path,
        &meta.slug,
        &meta.version,
        whitespace,
    )?;

    let patch_categories: HashSet<FileCategory> = [FileCategory::Source, FileCategory::Metadata]
        .into_iter()
        .collect();
    let filtered = diff_result.apply(&patch_categories, &[]);
    let has_customizations = !filtered.files.is_empty();

    if has_customizations {
        let total_ins: usize = filtered.files.iter().map(|f| f.insertions).sum();
        let total_del: usize = filtered.files.iter().map(|f| f.deletions).sum();
        println!(
            "  Found {} customized files ({} insertions, {} deletions)",
            filtered.files.len().to_string().bold(),
            total_ins.to_string().green(),
            total_del.to_string().red(),
        );
        for f in &filtered.files {
            println!("    {} {}", "~".yellow(), f.path);
        }
    } else {
        println!("  No local customizations found.");
    }

    let resolved_version = if let Some(v) = target_version {
        v.to_string()
    } else {
        info!("Checking latest version...");
        let info = source::fetch_plugin_versions(&meta.slug)?;
        info.latest_version
    };

    if resolved_version == meta.version {
        println!(
            "\n  {} Already on the latest version (v{}).",
            "✓".green().bold(),
            meta.version
        );
        return Ok(());
    }

    info!("Downloading v{resolved_version}...");
    let new_upstream = registry.fetch(&meta, Some(&resolved_version))?;
    let new_version = &resolved_version;

    println!(
        "\n  Upgrading: {} → {}",
        format!("v{}", meta.version).red(),
        format!("v{new_version}").green(),
    );

    let patch_text = if has_customizations {
        let mut patch = String::new();
        for file in &filtered.files {
            if !file.diff_text.is_empty() {
                patch.push_str(&file.diff_text);
            }
        }
        Some(patch)
    } else {
        None
    };

    let staging_dir = tempfile::TempDir::new().context("Failed to create staging directory")?;
    let staged_plugin = staging_dir.path().join(&meta.slug);
    copy_dir_recursive(&new_upstream.path, &staged_plugin)?;

    let mut patch_clean = true;
    let mut has_conflicts = false;
    let mut rej_content = String::new();
    let mut fuzzy_files: Vec<(String, f64)> = Vec::new();
    let patch_file_path = format!("{}-{}-customizations.patch", meta.slug, meta.version);

    if let Some(ref patch_text) = patch_text {
        info!("Applying customizations to new version...");

        let patches = mpatch::parse_patches(patch_text)
            .map_err(|e| anyhow::anyhow!("Failed to parse patch: {e:?}"))?;

        let options = mpatch::ApplyOptions::builder().fuzz_factor(0.6).build();

        let batch = mpatch::apply_patches_to_dir(&patches, &staged_plugin, options);

        let mut applied_exact = 0;
        let mut applied_fuzzy = 0;
        let mut failed = 0;

        for (file_path, result) in &batch.results {
            let file_str = file_path.to_string_lossy();

            match result {
                Ok(patch_result) => {
                    let failures = patch_result.report.failures();
                    let mut file_has_fuzzy = false;
                    let mut worst_score = 1.0_f64;

                    for status in &patch_result.report.hunk_results {
                        if let mpatch::HunkApplyStatus::Applied { match_type, .. } = status {
                            match match_type {
                                mpatch::MatchType::Exact
                                | mpatch::MatchType::ExactIgnoringWhitespace => {}
                                mpatch::MatchType::Fuzzy { score } => {
                                    file_has_fuzzy = true;
                                    has_conflicts = true;
                                    worst_score = worst_score.min(*score);
                                    println!(
                                        "    {} {} - hunk applied with fuzz ({:.0}% match, may be incorrect)",
                                        "⚠".yellow().bold(),
                                        file_str,
                                        score * 100.0,
                                    );
                                }
                            }
                        }
                    }

                    if failures.is_empty() {
                        if file_has_fuzzy {
                            applied_fuzzy += 1;
                            fuzzy_files.push((file_str.to_string(), worst_score));
                        } else {
                            applied_exact += 1;
                            debug!("  Applied cleanly: {file_str}");
                        }
                    } else {
                        failed += 1;
                        patch_clean = false;
                        println!(
                            "    {} {} - {} failed hunks",
                            "✗".red().bold(),
                            file_str,
                            failures.len(),
                        );

                        for failure in &failures {
                            use std::fmt::Write as _;
                            let _ = write!(
                                rej_content,
                                "--- Failed hunk #{} in {} ---\n{:?}\n\n",
                                failure.hunk_index + 1,
                                file_str,
                                failure.reason,
                            );
                        }
                    }
                }
                Err(e) => {
                    failed += 1;
                    patch_clean = false;
                    println!("    {} {} - {:?}", "✗".red().bold(), file_str, e);
                }
            }
        }

        let total_applied = applied_exact + applied_fuzzy;
        println!();
        if patch_clean && !has_conflicts {
            println!(
                "  {} All customizations applied cleanly ({total_applied} files)",
                "✓".green().bold(),
            );
        } else if patch_clean && has_conflicts {
            println!(
                "  {} {total_applied} files applied, but {} with fuzzy matching (review recommended)",
                "⚠".yellow().bold(),
                applied_fuzzy.to_string().yellow(),
            );
            println!("    Fuzzy matches may have applied your changes in the wrong location or");
            println!("    overwritten upstream changes. Review the diffs below.");
        } else {
            println!(
                "  {} {total_applied}/{} files applied, {} failed",
                "⚠".yellow().bold(),
                total_applied + failed,
                failed.to_string().red(),
            );
            if has_conflicts {
                println!(
                    "    Additionally, {} files applied with fuzzy matching (review recommended)",
                    applied_fuzzy.to_string().yellow(),
                );
            }
        }
    }

    if !fuzzy_files.is_empty() {
        let interactive = !yes && !dry_run && io::IsTerminal::is_terminal(&io::stdin());

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

        let mut review_content = String::new();

        for (rel_path, score) in &fuzzy_files {
            let upstream_file = new_upstream.path.join(rel_path);
            let patched_file = staged_plugin.join(rel_path);

            let upstream_text = fs::read_to_string(&upstream_file).unwrap_or_default();
            let patched_text = fs::read_to_string(&patched_file).unwrap_or_default();

            let diff = similar::TextDiff::from_lines(&upstream_text, &patched_text);
            let unified = diff
                .unified_diff()
                .header(
                    &format!("a/{rel_path} (upstream v{new_version})"),
                    &format!("b/{rel_path} (patched, {:.0}% fuzz match)", score * 100.0),
                )
                .context_radius(3)
                .to_string();

            if unified.is_empty() {
                continue;
            }

            review_content.push_str(&unified);

            if interactive {
                let resolved = resolve_file_hunks(rel_path, *score, &upstream_text, &patched_text)?;
                fs::write(&patched_file, resolved)
                    .with_context(|| format!("Failed to write resolved {rel_path}"))?;
            } else {
                println!(
                    "  {} {} ({:.0}% match)",
                    "diff".bold(),
                    rel_path,
                    score * 100.0,
                );
                output::write_colored_diff(&mut io::stdout().lock(), &unified, "  ")?;
            }
        }

        if !review_content.is_empty() {
            let review_path = format!("{}-{}-review.diff", meta.slug, meta.version);
            fs::write(&review_path, &review_content)
                .with_context(|| format!("Failed to write {review_path}"))?;
            println!("  Saved review diff: {}", review_path.bold());
        }
    }

    if dry_run {
        println!();
        if !has_customizations {
            println!(
                "  {} Dry run: upgrade v{} → v{} would apply cleanly (no customizations).",
                "✓".green().bold(),
                meta.version,
                new_version,
            );
        } else if patch_clean && !has_conflicts {
            println!(
                "  {} Dry run: all customizations would reapply cleanly after upgrade to v{new_version}.",
                "✓".green().bold(),
            );
        } else if patch_clean && has_conflicts {
            println!(
                "  {} Dry run: customizations would apply but {} with fuzzy matching.",
                "⚠".yellow().bold(),
                "some hunks matched inexactly".yellow(),
            );
            println!("    These files should be reviewed after upgrading to confirm correctness.");
        } else {
            println!(
                "  {} Dry run: some customizations would fail to reapply after upgrade to v{new_version}.",
                "⚠".yellow().bold(),
            );
        }
        return Ok(());
    }

    if !patch_clean {
        fs::write(&patch_file_path, patch_text.as_deref().unwrap_or(""))
            .with_context(|| format!("Failed to write {patch_file_path}"))?;
        println!("  Saved patch: {}", patch_file_path.bold());

        if !rej_content.is_empty() {
            let rej_path = format!("{}-{}.rej", meta.slug, meta.version);
            fs::write(&rej_path, &rej_content)
                .with_context(|| format!("Failed to write {rej_path}"))?;
            println!("  Saved rejected hunks: {}", rej_path.bold());
        }
    }

    if !yes {
        let prompt = if !patch_clean {
            "  Replace live plugin? (some customizations failed to apply)"
        } else if has_conflicts {
            "  Replace live plugin? (some hunks matched inexactly - review recommended)"
        } else {
            "  Replace live plugin with upgraded version?"
        };
        print!("{prompt} [y/N] ");
        io::stdout().flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        if !answer.trim().eq_ignore_ascii_case("y") {
            println!(
                "  Aborted. Staged version is in: {}",
                staged_plugin.display()
            );
            return Ok(());
        }
    }

    let backup_path = format!("{}-{}-backup.zip", meta.slug, meta.version);
    info!("Backing up current plugin to {backup_path}...");
    create_zip_backup(&meta.dir, &backup_path)?;

    info!("Replacing plugin directory...");
    fs::remove_dir_all(&meta.dir)
        .with_context(|| format!("Failed to remove {}", meta.dir.display()))?;
    copy_dir_recursive(&staged_plugin, &meta.dir)?;

    println!(
        "\n  {} {} upgraded to v{}",
        "✓".green().bold(),
        meta.name,
        new_version,
    );
    println!("  Backup: {backup_path}");

    Ok(())
}

#[allow(clippy::unnecessary_wraps)]
fn resolve_file_hunks(
    rel_path: &str,
    score: f64,
    upstream_text: &str,
    patched_text: &str,
) -> Result<String> {
    use similar::{ChangeTag, TextDiff};

    let diff = TextDiff::from_lines(upstream_text, patched_text);
    let groups = diff.grouped_ops(3);

    let upstream_lines: Vec<&str> = upstream_text.lines().collect();
    let patched_lines: Vec<&str> = patched_text.lines().collect();
    let mut result_lines: Vec<&str> = Vec::new();
    let mut last_upstream_idx = 0;

    println!(
        "  {} {} ({:.0}% match) — {} sections to review",
        "file".bold(),
        rel_path,
        score * 100.0,
        groups.len(),
    );

    for (group_idx, group) in groups.iter().enumerate() {
        let group_old_start = group.first().map_or(0, |op| op.old_range().start);
        let group_old_end = group.last().map_or(0, |op| op.old_range().end);
        let group_new_start = group.first().map_or(0, |op| op.new_range().start);
        let group_new_end = group.last().map_or(0, |op| op.new_range().end);

        result_lines.extend_from_slice(&upstream_lines[last_upstream_idx..group_old_start]);

        let has_changes = group
            .iter()
            .any(|op| !matches!(op.tag(), similar::DiffTag::Equal));

        if !has_changes {
            result_lines.extend_from_slice(&upstream_lines[group_old_start..group_old_end]);
            last_upstream_idx = group_old_end;
            continue;
        }

        println!(
            "\n    {} Section {}/{} (lines {}-{}):",
            "─".repeat(3).dimmed(),
            group_idx + 1,
            groups.len(),
            group_old_start + 1,
            group_old_end,
        );

        for op in group {
            for change in diff.iter_changes(op) {
                let line = change.to_string_lossy();
                match change.tag() {
                    ChangeTag::Delete => {
                        print!("      {}", format!("-{line}").red());
                    }
                    ChangeTag::Insert => {
                        print!("      {}", format!("+{line}").green());
                    }
                    ChangeTag::Equal => {
                        print!("      {}", format!(" {line}").dimmed());
                    }
                }
                if !line.ends_with('\n') {
                    println!();
                }
            }
        }

        let options = vec![
            "Keep our change (green lines)",
            "Use upstream only (discard green, keep red)",
            "Keep both (upstream + our additions)",
        ];

        let choice_idx = inquire::Select::new(&format!("  Section {}:", group_idx + 1), options)
            .with_starting_cursor(0)
            .raw_prompt()
            .map(|o| o.index)
            .unwrap_or(0);

        match choice_idx {
            0 => {
                result_lines.extend_from_slice(&patched_lines[group_new_start..group_new_end]);
                println!("      {} Keeping our change", "→".green());
            }
            1 => {
                result_lines.extend_from_slice(&upstream_lines[group_old_start..group_old_end]);
                println!("      {} Using upstream version", "→".yellow());
            }
            2 => {
                result_lines.extend_from_slice(&upstream_lines[group_old_start..group_old_end]);
                for op in group {
                    for change in diff.iter_changes(op) {
                        if change.tag() == ChangeTag::Insert
                            && let Some(idx) = change.new_index()
                            && let Some(line) = patched_lines.get(idx)
                        {
                            result_lines.push(line);
                        }
                    }
                }
                println!("      {} Keeping both versions", "→".cyan());
            }
            _ => {
                result_lines.extend_from_slice(&patched_lines[group_new_start..group_new_end]);
            }
        }

        last_upstream_idx = group_old_end;
    }

    result_lines.extend_from_slice(&upstream_lines[last_upstream_idx..]);

    let mut output = result_lines.join("\n");
    if upstream_text.ends_with('\n') || patched_text.ends_with('\n') {
        output.push('\n');
    }

    println!("    {} {} resolved\n", "✓".green().bold(), rel_path);
    Ok(output)
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in walkdir::WalkDir::new(src) {
        let entry = entry?;
        let rel = entry.path().strip_prefix(src)?;
        let target = dst.join(rel);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)?;
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

fn create_zip_backup(dir: &Path, zip_path: &str) -> Result<()> {
    let file =
        fs::File::create(zip_path).with_context(|| format!("Failed to create {zip_path}"))?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    for entry in walkdir::WalkDir::new(dir) {
        let entry = entry?;
        let rel = entry.path().strip_prefix(dir)?;
        let rel_str = rel.to_string_lossy();
        if rel_str.is_empty() {
            continue;
        }

        if entry.file_type().is_dir() {
            zip.add_directory(format!("{rel_str}/"), options)?;
        } else {
            zip.start_file(rel_str.to_string(), options)?;
            let mut f = fs::File::open(entry.path())?;
            std::io::copy(&mut f, &mut zip)?;
        }
    }

    zip.finish()?;
    Ok(())
}
