mod api_usage;
mod blocks;
mod burn_rate;
mod cache;
mod claude_binary;
mod claude_update;
mod config;
mod config_migration;
mod context;
mod entry_cache;
mod format;
mod http;
mod install;
mod paths;
mod pricing;
mod rate_limits;
mod timing;
mod types;

use anyhow::{Context, Result};
use blocks::find_active_block;
use burn_rate::calculate_burn_rate;
use cache::{cleanup_stale_locks, get_cache_dir, try_get_cached, update_cache};
use clap::{Parser, Subcommand};
use config::StatusElement;
use context::calculate_context;
use format::{
    BurnRateDisplay, format_api_metrics_group, format_block_info, format_burn_rate_component,
    format_context, format_directory, format_time_remaining_5h, format_time_remaining_7d,
    strip_emojis,
};
use paths::{find_claude_paths, iter_jsonl_files};
use pricing::PricingFetcher;
use std::fs;
use std::io::{self, ErrorKind, IsTerminal, Read};
use types::HookData;

#[derive(Parser)]
#[command(name = "ccusage-statusline-rs")]
#[command(version)]
#[command(about = "Claude Code usage statusline with live API integration", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Install statusLine configuration (respects CLAUDE_CONFIG_DIR)
    Install,
    /// Remove statusLine configuration (respects CLAUDE_CONFIG_DIR)
    Uninstall,
    /// Test the statusline with most recent transcript
    Test,
    /// Configure statusline elements and thresholds
    Config,
}

fn main() -> Result<()> {
    // Only fails if a provider is already installed, which cannot happen on the single
    // call in a fresh process.
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("rustls crypto provider already installed");

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Install) => install::install(),
        Some(Commands::Uninstall) => install::uninstall(),
        Some(Commands::Test) => run_test_mode(),
        Some(Commands::Config) => config::run_config_menu(),
        None => {
            let stdin = io::stdin();
            if stdin.is_terminal() {
                run_interactive_mode()
            } else {
                run_piped_mode()
            }
        }
    }
}

/// `session_id` arrives as untrusted stdin JSON and names the output-cache file;
/// a separator or `..` in it would place that file outside the cache dir.
fn cache_file_name(session_id: &str) -> String {
    let safe: String = session_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{}.lock", safe)
}

fn run_piped_mode() -> Result<()> {
    let mut input = String::new();
    io::stdin()
        .read_to_string(&mut input)
        .context("Failed to read stdin")?;

    if input.is_empty() {
        eprintln!("No input provided");
        std::process::exit(1);
    }

    let hook_data: HookData = serde_json::from_str(&input).context("Failed to parse JSON input")?;

    let cache_dir = get_cache_dir()?;
    let cache_path = cache_dir.join(cache_file_name(&hook_data.session_id));

    let statusline_config = config::StatuslineConfig::load_or_default();
    cleanup_stale_locks(
        &cache_dir,
        statusline_config
            .cache
            .output_cache_secs,
    );

    if let Some(cached) = try_get_cached(
        &cache_path,
        &hook_data.transcript_path,
        statusline_config
            .cache
            .output_cache_secs,
    )? {
        println!("{}", cached);
        return Ok(());
    }

    let output = generate_statusline(&hook_data, &statusline_config)?;
    println!("{}", output);

    // Cache update failure must not fail the process after output has been printed.
    // A missing transcript is expected (e.g. session not yet written to disk).
    if let Err(e) = update_cache(&cache_path, &hook_data.transcript_path, &output) {
        let is_not_found = e
            .chain()
            .any(|cause| {
                cause
                    .downcast_ref::<std::io::Error>()
                    .map(|io| io.kind() == ErrorKind::NotFound)
                    .unwrap_or(false)
            });
        if !is_not_found && std::io::stderr().is_terminal() {
            eprintln!("Output cache update failed: {:#}", e);
        }
    }

    Ok(())
}

fn run_interactive_mode() -> Result<()> {
    let statusline_config = config::StatuslineConfig::load_or_default();
    let hook_data = HookData {
        session_id: String::new(),
        transcript_path: String::new(),
        model: types::ModelInfo {
            id: None,
            display_name: String::new(),
        },
        workspace: None,
        context_window: None,
        rate_limits: None,
    };
    let output = generate_statusline(&hook_data, &statusline_config)?;
    println!("{}", output);
    Ok(())
}

fn run_test_mode() -> Result<()> {
    let claude_paths = find_claude_paths()?;

    let most_recent = iter_jsonl_files(&claude_paths)?
        .into_iter()
        .filter_map(|path| {
            fs::metadata(&path)
                .ok()
                .and_then(|m| {
                    m.modified()
                        .ok()
                })
                .map(|mtime| (path, mtime))
        })
        .max_by_key(|(_, mtime)| *mtime);

    let (transcript_path, _) =
        most_recent.context("No .jsonl files found in Claude directories")?;

    eprintln!("Testing with: {}", transcript_path.display());

    let hook_data = HookData {
        session_id: "test-session".to_string(),
        transcript_path: transcript_path
            .to_string_lossy()
            .to_string(),
        model: types::ModelInfo {
            id: None,
            display_name: "Opus 5".to_string(),
        },
        workspace: Some(types::Workspace {
            current_dir: std::env::current_dir()?
                .to_string_lossy()
                .to_string(),
        }),
        context_window: None,
        rate_limits: None,
    };

    let statusline_config = config::StatuslineConfig::load_or_default();
    let output = generate_statusline(&hook_data, &statusline_config)?;
    println!("{}", output);

    Ok(())
}

/// Generate statusline output
fn generate_statusline(
    hook_data: &HookData,
    statusline_config: &config::StatuslineConfig,
) -> Result<String> {
    let cache_dir = get_cache_dir()?;
    let plan_type = api_usage::get_plan_type();
    let thresholds = &statusline_config.thresholds;

    // The three fetches are independent and each is bounded by the HTTP client's own
    // timeout. Run serially, a blackholed network costs their sum; concurrently, the
    // render waits only for the slowest.
    let (api_result, pricing, update_available) = timing::phase("fetches", || {
        std::thread::scope(|scope| {
            let api = scope.spawn(|| {
                if statusline_config.needs_api() {
                    timing::phase("api", || api_usage::fetch_usage(&statusline_config.cache))
                } else {
                    api_usage::ApiUsageResult::Unavailable
                }
            });
            let pricing =
                scope.spawn(|| timing::phase("pricing", || PricingFetcher::new(&cache_dir)));
            let update = scope.spawn(|| {
                timing::phase("update", || {
                    claude_update::check_update_available(statusline_config)
                })
            });

            (api.join(), pricing.join(), update.join())
        })
    });

    // A panicked worker is a bug, not a runtime condition; surface it rather than
    // rendering a plausible-looking line with a silently missing element.
    let api_result = api_result.map_err(|_| anyhow::anyhow!("API usage worker panicked"))?;
    let pricing = pricing.map_err(|_| anyhow::anyhow!("pricing worker panicked"))??;
    let update_available =
        update_available.map_err(|_| anyhow::anyhow!("update-check worker panicked"))?;

    let polled_api_usage = api_result
        .data()
        .cloned();

    let api_usage = rate_limits::merge_and_get_effective_usage(
        hook_data
            .rate_limits
            .as_ref(),
        polled_api_usage,
    )?;

    let claude_paths = find_claude_paths()?;
    let five_hour_reset = api_usage
        .as_ref()
        .and_then(|a| {
            a.five_hour
                .as_ref()
        })
        .and_then(|w| w.resets_at);
    let block = timing::phase("block", || {
        find_active_block(&claude_paths, &pricing, &cache_dir, five_hour_reset)
    })?;
    let burn_rate = calculate_burn_rate(
        block.as_ref(),
        api_usage.as_ref(),
        thresholds.burn_rate_show_ratio(),
    )?;
    let context_info = timing::phase("context", || calculate_context(hook_data))?;

    let mut parts = Vec::new();
    let mut api_metrics_emitted = false;
    let mut burn_rate_emitted = false;
    let mut update_emitted = false;

    for element in &statusline_config.enabled_elements {
        match element {
            StatusElement::Model => {
                let name = hook_data
                    .model
                    .display_name
                    .replace(" context)", ")");
                if !name.is_empty() {
                    parts.push(format!("🤖{}", name));
                }
            }
            StatusElement::BlockCost => {
                parts.push(format!("💰{}", format_block_info(block.as_ref())));
            }
            StatusElement::TimeRemaining5h => {
                if let Some(time) =
                    format_time_remaining_5h(block.as_ref(), api_usage.as_ref(), plan_type)
                {
                    parts.push(time);
                }
            }
            StatusElement::TimeRemaining7d => {
                if let Some(time) = format_time_remaining_7d(api_usage.as_ref(), plan_type) {
                    parts.push(time);
                }
            }
            StatusElement::BurnRate | StatusElement::BurnRateEta => {
                if !burn_rate_emitted {
                    burn_rate_emitted = true;
                    let enabled = &statusline_config.enabled_elements;
                    let has_rate = enabled.contains(&StatusElement::BurnRate);
                    let has_eta = enabled.contains(&StatusElement::BurnRateEta);
                    if let Some(s) =
                        BurnRateDisplay::from_elements(has_rate, has_eta).and_then(|display| {
                            format_burn_rate_component(&burn_rate, plan_type, display, thresholds)
                        })
                    {
                        parts.push(s);
                    }
                }
            }
            StatusElement::Context => {
                if let Some(ctx) = context_info.as_ref() {
                    parts.push(format!("🧠{}", format_context(ctx, thresholds)));
                }
            }
            StatusElement::ApiMetrics5h
            | StatusElement::ApiMetrics7d
            | StatusElement::ApiMetricsModel7d => {
                if !api_metrics_emitted {
                    api_metrics_emitted = true;
                    if let Some(s) = format_api_metrics_group(
                        &statusline_config.enabled_elements,
                        api_result.error_label(),
                        api_usage.as_ref(),
                    ) {
                        parts.push(s);
                    }
                }
            }
            StatusElement::UpdateStable | StatusElement::UpdateLatest => {
                if !update_emitted {
                    update_emitted = true;
                    if let Some(ref new_version) = update_available {
                        parts.push(format!("🔼{}", new_version));
                    }
                }
            }
            StatusElement::Directory => {
                if let Some(workspace) = &hook_data.workspace {
                    parts.push(format_directory(&workspace.current_dir));
                }
            }
        }
    }

    let output = parts.join(" │ ");
    if statusline_config.show_emojis {
        Ok(output)
    } else {
        Ok(strip_emojis(&output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_performance_under_20ms() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let _ = run_interactive_mode();

        let iterations = 10;
        let mut total_duration = std::time::Duration::ZERO;

        for _ in 0..iterations {
            let start = std::time::Instant::now();
            let _ = run_interactive_mode();
            total_duration += start.elapsed();
        }

        let avg_ms = total_duration.as_millis() / iterations as u128;
        eprintln!("Average execution time: {}ms (cached)", avg_ms);
        let threshold = if cfg!(debug_assertions) { 100 } else { 20 };
        assert!(
            avg_ms <= threshold,
            "Average {}ms exceeds {}ms target",
            avg_ms,
            threshold
        );
    }
}
