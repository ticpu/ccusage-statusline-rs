mod api_usage;
mod blocks;
mod cache;
mod claude_binary;
mod claude_update;
mod config;
mod format;
mod install;
mod pricing;
mod types;

use anyhow::{Context, Result};
use blocks::find_active_block;
use cache::{get_cache_dir, try_get_cached, update_cache};
use chrono::Utc;
use clap::{Parser, Subcommand};
use format::*;
use pricing::PricingFetcher;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, IsTerminal, Read};
use std::path::PathBuf;
use types::{BurnRate, ClaudeConfig, ContextInfo, HookData, UsageData};

/// Context limit when autoCompactEnabled=true (Claude compacts context before reaching 100%)
const COMPACTED_CONTEXT_LIMIT: u64 = 155_000;

/// Context limit when autoCompactEnabled=false (full 200k nominal limit)
const FULL_CONTEXT_LIMIT: u64 = 200_000;

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
    /// Install statusLine configuration to ~/.claude/settings.json
    Install,
    /// Remove statusLine configuration from ~/.claude/settings.json
    Uninstall,
    /// Test the statusline with most recent transcript
    Test,
    /// Configure statusline elements (enable/disable and reorder)
    Config,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Handle subcommands
    match cli.command {
        Some(Commands::Install) => {
            install::install()?;
            Ok(())
        }
        Some(Commands::Uninstall) => {
            install::uninstall()?;
            Ok(())
        }
        Some(Commands::Test) => run_test_mode(),
        Some(Commands::Config) => config::run_config_menu(),
        None => {
            // No subcommand, run normal mode (piped or interactive)
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
        eprintln!("❌ No input provided");
        std::process::exit(1);
    }

    let hook_data: HookData = serde_json::from_str(&input).context("Failed to parse JSON input")?;

    let cache_dir = get_cache_dir()?;
    fs::create_dir_all(&cache_dir).context("Failed to create cache directory")?;

    let cache_path = cache_dir.join(format!("{}.lock", hook_data.session_id));

    if let Some(cached) = try_get_cached(&cache_path, &hook_data.transcript_path)? {
        println!("{}", cached);
        return Ok(());
    }

    let output = generate_statusline(&hook_data)?;
    println!("{}", output);

    update_cache(&cache_path, &hook_data.transcript_path, &output)?;

    Ok(())
}

fn run_interactive_mode() -> Result<()> {
    let cache_dir = get_cache_dir()?;
    fs::create_dir_all(&cache_dir).context("Failed to create cache directory")?;

    let api_result = api_usage::fetch_usage();
    let api_usage = api_result.data().cloned();

    let pricing = PricingFetcher::new(&cache_dir)?;
    let claude_paths = find_claude_paths()?;
    let block = find_active_block(&claude_paths, &pricing)?;
    let burn_rate = calculate_burn_rate(&block)?;

    let mut parts = Vec::new();

    parts.push(format!("💰{}", format_block_info(&block)));

    if let Some(time) = format_time_remaining_5h(&block, &api_usage) {
        parts.push(time);
    }

    parts.push(format!("🔥{}", format_burn_rate(&burn_rate)));

    if api_result.is_stale() {
        parts.push("📊(api error)".to_string());
    } else if let Some(api) = format_api_usage_5h(&api_usage) {
        parts.push(format!("📊{}", api));
        if let Some(api) = format_api_usage_7d(&api_usage) {
            parts.push(api);
        }
    }

    println!("{}", parts.join(" │ "));

    Ok(())
}

fn run_test_mode() -> Result<()> {
    let claude_paths = find_claude_paths()?;

    let mut most_recent: Option<(PathBuf, std::time::SystemTime)> = None;

    for base_path in &claude_paths {
        let project_dirs = fs::read_dir(base_path)
            .with_context(|| format!("Failed to read directory: {}", base_path.display()))?;

        for project_entry in project_dirs.flatten() {
            let project_path = project_entry.path();
            if !project_path.is_dir() {
                continue;
            }

            let session_files = match fs::read_dir(&project_path) {
                Ok(entries) => entries,
                Err(_) => continue,
            };

            for session_entry in session_files.flatten() {
                let session_path = session_entry.path();
                if session_path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                    continue;
                }

                if let Ok(metadata) = fs::metadata(&session_path)
                    && let Ok(modified) = metadata.modified()
                    && (most_recent.is_none() || modified > most_recent.as_ref().unwrap().1)
                {
                    most_recent = Some((session_path, modified));
                }
            }
        }
    }

    let (transcript_path, _) =
        most_recent.context("No .jsonl files found in Claude directories")?;

    eprintln!("Testing with: {}", transcript_path.display());

    let hook_data = HookData {
        session_id: "test-session".to_string(),
        transcript_path: transcript_path.to_string_lossy().to_string(),
        model: types::ModelInfo {
            id: "claude-sonnet-4-20250514".to_string(),
            display_name: "Claude 3.5 Sonnet".to_string(),
        },
        workspace: Some(types::Workspace {
            current_dir: std::env::current_dir()?.to_string_lossy().to_string(),
        }),
    };

    let output = generate_statusline(&hook_data)?;
    println!("{}", output);

    Ok(())
}

/// Generate statusline output
fn generate_statusline(hook_data: &HookData) -> Result<String> {
    let cache_dir = get_cache_dir()?;
    fs::create_dir_all(&cache_dir).context("Failed to create cache directory")?;

    let statusline_config = config::StatuslineConfig::load().unwrap_or_default();
    let api_result = api_usage::fetch_usage();
    let api_usage = api_result.data().cloned();

    let pricing = PricingFetcher::new(&cache_dir)?;
    let claude_paths = find_claude_paths()?;
    let block = find_active_block(&claude_paths, &pricing)?;
    let burn_rate = calculate_burn_rate(&block)?;
    let context_info = calculate_context_tokens(&hook_data.transcript_path, &hook_data.model.id)?;
    let update_available = claude_update::check_update_available();

    let mut parts = Vec::new();

    for element in &statusline_config.enabled_elements {
        match element {
            config::StatusElement::Model => {
                parts.push(format!("🤖{}", hook_data.model.display_name));
            }
            config::StatusElement::BlockCost => {
                parts.push(format!("💰{}", format_block_info(&block)));
            }
            config::StatusElement::TimeRemaining5h => {
                if let Some(time) = format_time_remaining_5h(&block, &api_usage) {
                    parts.push(time);
                }
            }
            config::StatusElement::TimeRemaining7d => {
                if let Some(time) = format_time_remaining_7d(&api_usage) {
                    parts.push(time);
                }
            }
            config::StatusElement::BurnRate => {
                parts.push(format!("🔥{}", format_burn_rate(&burn_rate)));
            }
            config::StatusElement::Context => {
                parts.push(format!("🧠{}", format_context(&context_info)));
            }
            config::StatusElement::ApiMetrics5h => {
                if api_result.is_stale() {
                    parts.push("📊(api error)".to_string());
                } else {
                    // Group all API metrics together with space separator
                    let mut api_parts = Vec::new();
                    if let Some(api) = format_api_usage_5h(&api_usage) {
                        api_parts.push(format!("📊{}", api));
                    }
                    if statusline_config
                        .enabled_elements
                        .contains(&config::StatusElement::ApiMetrics7d)
                        && let Some(api) = format_api_usage_7d(&api_usage)
                    {
                        api_parts.push(api);
                    }
                    if statusline_config
                        .enabled_elements
                        .contains(&config::StatusElement::ApiMetricsSonnet)
                        && let Some(api) = format_api_usage_sonnet(&api_usage)
                    {
                        api_parts.push(api);
                    }
                    if !api_parts.is_empty() {
                        parts.push(api_parts.join(" "));
                    }
                }
            }
            config::StatusElement::ApiMetrics7d => {
                // Only handle if 5h not enabled (otherwise handled above)
                if !statusline_config
                    .enabled_elements
                    .contains(&config::StatusElement::ApiMetrics5h)
                {
                    if api_result.is_stale() {
                        parts.push("📊(api error)".to_string());
                    } else {
                        let mut api_parts = Vec::new();
                        if let Some(api) = format_api_usage_7d(&api_usage) {
                            api_parts.push(format!("📊{}", api));
                        }
                        if statusline_config
                            .enabled_elements
                            .contains(&config::StatusElement::ApiMetricsSonnet)
                            && let Some(api) = format_api_usage_sonnet(&api_usage)
                        {
                            api_parts.push(api);
                        }
                        if !api_parts.is_empty() {
                            parts.push(api_parts.join(" "));
                        }
                    }
                }
            }
            config::StatusElement::ApiMetricsSonnet => {
                // Only handle if neither 5h nor 7d enabled
                if !statusline_config
                    .enabled_elements
                    .contains(&config::StatusElement::ApiMetrics5h)
                    && !statusline_config
                        .enabled_elements
                        .contains(&config::StatusElement::ApiMetrics7d)
                {
                    if api_result.is_stale() {
                        parts.push("📊(api error)".to_string());
                    } else if let Some(api) = format_api_usage_sonnet(&api_usage) {
                        parts.push(format!("📊{}", api));
                    }
                }
            }
            config::StatusElement::UpdateStable | config::StatusElement::UpdateLatest => {
                if let Some(ref new_version) = update_available {
                    parts.push(format!("🔼{}", new_version));
                }
            }
            config::StatusElement::Directory => {
                if let Some(workspace) = &hook_data.workspace {
                    parts.push(format_directory(&workspace.current_dir));
                }
            }
        }
    }

    Ok(parts.join(" │ "))
}

/// Find Claude data directories
fn find_claude_paths() -> Result<Vec<PathBuf>> {
    let home = std::env::var("HOME").context("HOME not set")?;
    let mut paths = Vec::new();

    // Check both old and new default paths
    let old_path = PathBuf::from(&home).join(".claude/projects");
    let new_path = PathBuf::from(&home).join(".config/claude/projects");

    if old_path.exists() {
        paths.push(old_path);
    }
    if new_path.exists() {
        paths.push(new_path);
    }

    if paths.is_empty() {
        anyhow::bail!("No Claude data directories found");
    }

    Ok(paths)
}

/// Get effective context limit based on autoCompactEnabled setting
fn get_context_limit() -> u64 {
    let home = match std::env::var("HOME") {
        Ok(h) => h,
        Err(_) => return COMPACTED_CONTEXT_LIMIT,
    };

    let config_path = PathBuf::from(&home).join(".claude.json");

    // Try to read and parse config
    match fs::read_to_string(&config_path) {
        Ok(content) => match serde_json::from_str::<ClaudeConfig>(&content) {
            Ok(config) => {
                if config.auto_compact_enabled {
                    COMPACTED_CONTEXT_LIMIT
                } else {
                    FULL_CONTEXT_LIMIT
                }
            }
            Err(_) => COMPACTED_CONTEXT_LIMIT,
        },
        Err(_) => COMPACTED_CONTEXT_LIMIT,
    }
}

/// Calculate burn rate for a block
fn calculate_burn_rate(block: &types::Block) -> Result<BurnRate> {
    if !block.is_active {
        return Ok(BurnRate {
            cost_per_hour: 0.0,
            tokens_per_minute: 0,
        });
    }

    let now = Utc::now();
    let elapsed = (now - block.start_time).num_minutes() as f64;

    if elapsed <= 0.0 {
        return Ok(BurnRate {
            cost_per_hour: 0.0,
            tokens_per_minute: 0,
        });
    }

    let cost_per_hour = (block.cost_usd / elapsed) * 60.0;
    let tokens_per_minute = ((block.total_tokens as f64) / elapsed) as u64;

    Ok(BurnRate {
        cost_per_hour,
        tokens_per_minute,
    })
}

/// Calculate context tokens from transcript
fn calculate_context_tokens(transcript_path: &str, _model_id: &str) -> Result<Option<ContextInfo>> {
    // Read last message from transcript to get current context
    let file = match File::open(transcript_path) {
        Ok(f) => f,
        Err(_) => return Ok(None),
    };

    let reader = BufReader::new(file);
    let mut last_tokens: Option<u64> = None;

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<UsageData>(&line) {
            // Calculate total context including cached tokens
            let context = entry.message.usage.input_tokens
                + entry.message.usage.cache_creation_input_tokens
                + entry.message.usage.cache_read_input_tokens;
            last_tokens = Some(context);
        }
    }

    let total_tokens = last_tokens.unwrap_or(0);

    let context_limit = get_context_limit();
    let percentage = ((total_tokens as f64 / context_limit as f64) * 100.0).min(100.0) as u32;

    Ok(Some(ContextInfo {
        tokens: total_tokens,
        percentage,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_performance_under_20ms() {
        // Warmup (populates caches)
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
        // CI has no OAuth credentials so API returns immediately
        let threshold = if cfg!(debug_assertions) { 100 } else { 20 };
        assert!(
            avg_ms <= threshold,
            "Average {}ms exceeds {}ms target",
            avg_ms,
            threshold
        );
    }

    #[test]
    fn test_context_calculation_with_caching_compacted() {
        // Test with compacted limit (155k)
        let tokens = 10 + 500 + 95000;
        let percentage =
            ((tokens as f64 / COMPACTED_CONTEXT_LIMIT as f64) * 100.0).min(100.0) as u32;

        assert_eq!(tokens, 95510);
        assert_eq!(percentage, 61); // 95510 / 155000 * 100 = 61.62 -> 61
    }

    #[test]
    fn test_context_calculation_with_caching_full() {
        // Test with full limit (200k)
        let tokens = 10 + 500 + 95000;
        let percentage = ((tokens as f64 / FULL_CONTEXT_LIMIT as f64) * 100.0).min(100.0) as u32;

        assert_eq!(tokens, 95510);
        assert_eq!(percentage, 47); // 95510 / 200000 * 100 = 47.755 -> 47
    }

    #[test]
    fn test_context_calculation_without_caching_compacted() {
        let tokens = 1000;
        let percentage =
            ((tokens as f64 / COMPACTED_CONTEXT_LIMIT as f64) * 100.0).min(100.0) as u32;

        assert_eq!(tokens, 1000);
        assert_eq!(percentage, 0); // 1000 / 155000 * 100 = 0.64 -> 0
    }

    #[test]
    fn test_context_calculation_without_caching_full() {
        let tokens = 1000;
        let percentage = ((tokens as f64 / FULL_CONTEXT_LIMIT as f64) * 100.0).min(100.0) as u32;

        assert_eq!(tokens, 1000);
        assert_eq!(percentage, 0); // 1000 / 200000 * 100 = 0.5 -> 0
    }

    #[test]
    fn test_context_calculation_capped_compacted() {
        // Test that percentage caps at 100% with compacted limit
        let tokens = 199_000u64;
        let percentage =
            ((tokens as f64 / COMPACTED_CONTEXT_LIMIT as f64) * 100.0).min(100.0) as u32;

        assert_eq!(percentage, 100); // 199000 / 155000 * 100 = 128.38 -> capped at 100
    }

    #[test]
    fn test_context_calculation_capped_full() {
        // Test that percentage caps at 100% with full limit
        let tokens = 250_000u64;
        let percentage = ((tokens as f64 / FULL_CONTEXT_LIMIT as f64) * 100.0).min(100.0) as u32;

        assert_eq!(percentage, 100); // 250000 / 200000 * 100 = 125 -> capped at 100
    }
}
