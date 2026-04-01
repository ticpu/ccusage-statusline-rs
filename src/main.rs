mod api_usage;
mod blocks;
mod burn_rate;
mod cache;
mod claude_binary;
mod claude_update;
mod config;
mod context;
mod format;
mod install;
mod paths;
mod pricing;
mod types;

use anyhow::{Context, Result};
use blocks::find_active_block;
use burn_rate::calculate_burn_rate;
use cache::{cleanup_stale_locks, get_cache_dir, try_get_cached, update_cache};
use clap::{Parser, Subcommand};
use config::StatusElement;
use context::calculate_context;
use format::*;
use paths::{find_claude_paths, iter_jsonl_files};
use pricing::PricingFetcher;
use std::fs;
use std::io::{self, IsTerminal, Read};
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
    /// Configure statusline elements (enable/disable and reorder)
    Config,
}

fn main() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

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
    fs::create_dir_all(&cache_dir).context("Failed to create cache directory")?;

    let cache_path = cache_dir.join(format!("{}.lock", hook_data.session_id));

    let statusline_config = config::StatuslineConfig::load().unwrap_or_default();
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

    update_cache(&cache_path, &hook_data.transcript_path, &output)?;

    Ok(())
}

fn run_interactive_mode() -> Result<()> {
    let cache_dir = get_cache_dir()?;
    fs::create_dir_all(&cache_dir).context("Failed to create cache directory")?;

    let statusline_config = config::StatuslineConfig::load().unwrap_or_default();
    let thresholds = &statusline_config.thresholds;
    let plan_type = api_usage::get_plan_type();
    let api_result = if statusline_config.needs_api() {
        api_usage::fetch_usage(&statusline_config.cache)
    } else {
        api_usage::ApiUsageResult::Unavailable
    };
    let api_usage = api_result
        .data()
        .cloned();

    let pricing = PricingFetcher::new(&cache_dir)?;
    let claude_paths = find_claude_paths()?;
    let block = find_active_block(&claude_paths, &pricing)?;
    let burn_rate = calculate_burn_rate(
        &block,
        api_usage.as_ref(),
        thresholds.burn_rate_show_ratio(),
    )?;

    let mut parts = Vec::new();

    parts.push(format!("💰{}", format_block_info(&block)));

    if let Some(time) = format_time_remaining_5h(&block, api_usage.as_ref(), plan_type) {
        parts.push(time);
    }

    if let Some(s) = format_burn_rate_component(&burn_rate, plan_type, true, false, thresholds) {
        parts.push(s);
    }

    if let Some(label) = api_result.error_label() {
        parts.push(format!("📊({})", label));
    } else if let Some(api) = format_api_usage_5h(api_usage.as_ref()) {
        parts.push(format!("📊{}", api));
        if let Some(api) = format_api_usage_7d(api_usage.as_ref()) {
            parts.push(api);
        }
    }

    let output = parts.join(" │ ");
    if statusline_config.show_emojis {
        println!("{}", output);
    } else {
        println!("{}", strip_emojis(&output));
    }

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
            display_name: "Claude 3.5 Sonnet".to_string(),
        },
        workspace: Some(types::Workspace {
            current_dir: std::env::current_dir()?
                .to_string_lossy()
                .to_string(),
        }),
        context_window: None,
    };

    let statusline_config = config::StatuslineConfig::load().unwrap_or_default();
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
    fs::create_dir_all(&cache_dir).context("Failed to create cache directory")?;

    let plan_type = api_usage::get_plan_type();
    let thresholds = &statusline_config.thresholds;
    let api_result = if statusline_config.needs_api() {
        api_usage::fetch_usage(&statusline_config.cache)
    } else {
        api_usage::ApiUsageResult::Unavailable
    };
    let api_usage = api_result
        .data()
        .cloned();

    let pricing = PricingFetcher::new(&cache_dir)?;
    let claude_paths = find_claude_paths()?;
    let block = find_active_block(&claude_paths, &pricing)?;
    let burn_rate = calculate_burn_rate(
        &block,
        api_usage.as_ref(),
        thresholds.burn_rate_show_ratio(),
    )?;
    let context_info = calculate_context(hook_data)?;
    let update_available = claude_update::check_update_available();

    let mut parts = Vec::new();
    let mut api_metrics_emitted = false;
    let mut burn_rate_emitted = false;

    for element in &statusline_config.enabled_elements {
        match element {
            StatusElement::Model => {
                let name = hook_data
                    .model
                    .display_name
                    .replace(" context)", ")");
                parts.push(format!("🤖{}", name));
            }
            StatusElement::BlockCost => {
                parts.push(format!("💰{}", format_block_info(&block)));
            }
            StatusElement::TimeRemaining5h => {
                if let Some(time) = format_time_remaining_5h(&block, api_usage.as_ref(), plan_type)
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
                    let show_rate = enabled.contains(&StatusElement::BurnRate);
                    let show_eta = enabled.contains(&StatusElement::BurnRateEta);
                    if let Some(s) = format_burn_rate_component(
                        &burn_rate, plan_type, show_rate, show_eta, thresholds,
                    ) {
                        parts.push(s);
                    }
                }
            }
            StatusElement::Context => {
                parts.push(format!(
                    "🧠{}",
                    format_context(context_info.as_ref(), thresholds)
                ));
            }
            StatusElement::ApiMetrics5h
            | StatusElement::ApiMetrics7d
            | StatusElement::ApiMetricsSonnet => {
                if !api_metrics_emitted {
                    api_metrics_emitted = true;
                    if let Some(label) = api_result.error_label() {
                        parts.push(format!("📊({})", label));
                    } else {
                        let enabled = &statusline_config.enabled_elements;
                        let mut api_parts = Vec::new();

                        if enabled.contains(&StatusElement::ApiMetrics5h)
                            && let Some(text) = format_api_usage_5h(api_usage.as_ref())
                        {
                            api_parts.push(format!("📊{}", text));
                        }
                        if enabled.contains(&StatusElement::ApiMetrics7d)
                            && let Some(text) = format_api_usage_7d(api_usage.as_ref())
                        {
                            if api_parts.is_empty() {
                                api_parts.push(format!("📊{}", text));
                            } else {
                                api_parts.push(text);
                            }
                        }
                        if enabled.contains(&StatusElement::ApiMetricsSonnet)
                            && let Some(text) = format_api_usage_sonnet(api_usage.as_ref())
                        {
                            if api_parts.is_empty() {
                                api_parts.push(format!("📊{}", text));
                            } else {
                                api_parts.push(text);
                            }
                        }
                        if !api_parts.is_empty() {
                            parts.push(api_parts.join(" "));
                        }
                    }
                }
            }
            StatusElement::UpdateStable | StatusElement::UpdateLatest => {
                if let Some(ref new_version) = update_available {
                    parts.push(format!("🔼{}", new_version));
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
