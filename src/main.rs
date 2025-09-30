mod blocks;
mod cache;
mod format;
mod pricing;
mod types;

use anyhow::{Context, Result};
use blocks::find_active_block;
use cache::{get_cache_dir, try_get_cached, update_cache};
use chrono::Utc;
use format::{format_block_info, format_burn_rate, format_context};
use pricing::PricingFetcher;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read};
use std::path::PathBuf;
use types::{BurnRate, ContextInfo, HookData, UsageData};

fn main() -> Result<()> {
    // Read input from stdin
    let mut input = String::new();
    io::stdin()
        .read_to_string(&mut input)
        .context("Failed to read stdin")?;

    if input.is_empty() {
        eprintln!("❌ No input provided");
        std::process::exit(1);
    }

    let hook_data: HookData = serde_json::from_str(&input).context("Failed to parse JSON input")?;

    // Get cache directory from XDG_RUNTIME_DIR
    let cache_dir = get_cache_dir()?;
    fs::create_dir_all(&cache_dir).context("Failed to create cache directory")?;

    let cache_path = cache_dir.join(format!("{}.lock", hook_data.session_id));

    // Try to get cached output
    if let Some(cached) = try_get_cached(&cache_path, &hook_data.transcript_path)? {
        println!("{}", cached);
        return Ok(());
    }

    // Generate fresh output
    let output = generate_statusline(&hook_data)?;
    println!("{}", output);

    // Update cache
    update_cache(&cache_path, &hook_data.transcript_path, &output)?;

    Ok(())
}

/// Generate statusline output
fn generate_statusline(hook_data: &HookData) -> Result<String> {
    // Get cache directory for pricing data
    let cache_dir = get_cache_dir()?;
    fs::create_dir_all(&cache_dir).context("Failed to create cache directory")?;

    // Initialize pricing fetcher (loads or fetches LiteLLM pricing)
    let pricing = PricingFetcher::new(&cache_dir)?;

    // Find Claude data directories
    let claude_paths = find_claude_paths()?;

    // Load usage data and find active block
    let block = find_active_block(&claude_paths, &pricing)?;

    // Calculate burn rate
    let burn_rate = calculate_burn_rate(&block)?;

    // Calculate context tokens
    let context_info = calculate_context_tokens(&hook_data.transcript_path, &hook_data.model.id)?;

    // Format output
    let block_info = format_block_info(&block);
    let burn_info = format_burn_rate(&burn_rate);
    let context_str = format_context(&context_info);

    Ok(format!(
        "🤖 {} | 💰 {} | 🔥 {} | 🧠 {}",
        hook_data.model.display_name, block_info, burn_info, context_str
    ))
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
    // Parse transcript JSONL and count tokens
    let file = match File::open(transcript_path) {
        Ok(f) => f,
        Err(_) => return Ok(None),
    };

    let reader = BufReader::new(file);
    let mut total_tokens = 0u64;

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<UsageData>(&line) {
            total_tokens += entry.message.usage.input_tokens;
        }
    }

    // Simplified context window (200k for Sonnet 4)
    let context_limit = 200_000u64;
    let percentage = ((total_tokens as f64 / context_limit as f64) * 100.0) as u32;

    Ok(Some(ContextInfo {
        tokens: total_tokens,
        percentage,
    }))
}