use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Timelike, Utc};
use fs2::FileExt;
use owo_colors::OwoColorize;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

/// Hook input data from Claude Code
#[derive(Debug, Deserialize)]
struct HookData {
    session_id: String,
    transcript_path: String,
    model: ModelInfo,
}

#[derive(Debug, Deserialize)]
struct ModelInfo {
    id: String,
    display_name: String,
}

/// Usage data entry from JSONL
#[derive(Debug, Deserialize)]
struct UsageData {
    timestamp: String,
    message: MessageData,
    #[serde(default, rename = "requestId")]
    request_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MessageData {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    id: Option<String>,
    usage: UsageTokens,
}

#[derive(Debug, Deserialize)]
struct UsageTokens {
    input_tokens: u64,
    output_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
}

/// LiteLLM Model Pricing (matching TypeScript schema)
#[derive(Debug, Deserialize, Serialize, Clone)]
struct ModelPricing {
    #[serde(default)]
    input_cost_per_token: Option<f64>,
    #[serde(default)]
    output_cost_per_token: Option<f64>,
    #[serde(default)]
    cache_creation_input_token_cost: Option<f64>,
    #[serde(default)]
    cache_read_input_token_cost: Option<f64>,
    #[serde(default)]
    input_cost_per_token_above_200k_tokens: Option<f64>,
    #[serde(default)]
    output_cost_per_token_above_200k_tokens: Option<f64>,
    #[serde(default)]
    cache_creation_input_token_cost_above_200k_tokens: Option<f64>,
    #[serde(default)]
    cache_read_input_token_cost_above_200k_tokens: Option<f64>,
}

impl ModelPricing {
    const THRESHOLD: u64 = 200_000;

    /// Calculate cost with tiered pricing
    fn calculate_tiered_cost(
        &self,
        tokens: u64,
        base_price: Option<f64>,
        tiered_price: Option<f64>,
    ) -> f64 {
        if tokens == 0 {
            return 0.0;
        }

        let base = base_price.unwrap_or(0.0);

        if tokens <= Self::THRESHOLD {
            tokens as f64 * base
        } else {
            let tiered = tiered_price.unwrap_or(base);
            (Self::THRESHOLD as f64 * base) + ((tokens - Self::THRESHOLD) as f64 * tiered)
        }
    }

    /// Calculate total cost for a usage entry
    fn calculate_cost(&self, usage: &UsageTokens) -> f64 {
        let input_cost = self.calculate_tiered_cost(
            usage.input_tokens,
            self.input_cost_per_token,
            self.input_cost_per_token_above_200k_tokens,
        );

        let output_cost = self.calculate_tiered_cost(
            usage.output_tokens,
            self.output_cost_per_token,
            self.output_cost_per_token_above_200k_tokens,
        );

        let cache_write_cost = self.calculate_tiered_cost(
            usage.cache_creation_input_tokens,
            self.cache_creation_input_token_cost,
            self.cache_creation_input_token_cost_above_200k_tokens,
        );

        let cache_read_cost = self.calculate_tiered_cost(
            usage.cache_read_input_tokens,
            self.cache_read_input_token_cost,
            self.cache_read_input_token_cost_above_200k_tokens,
        );

        input_cost + output_cost + cache_write_cost + cache_read_cost
    }
}

/// Cached pricing data with timestamp
#[derive(Debug, Deserialize, Serialize)]
struct PricingCache {
    timestamp: i64,
    models: HashMap<String, ModelPricing>,
}

/// Pricing fetcher with caching
struct PricingFetcher {
    models: HashMap<String, ModelPricing>,
}

impl PricingFetcher {
    const LITELLM_URL: &'static str = "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";
    const MAX_AGE_SECONDS: i64 = 86400; // 24 hours

    /// Create a new pricing fetcher and load pricing data
    fn new(cache_dir: PathBuf) -> Result<Self> {
        let models = Self::load_pricing(&cache_dir)?;
        Ok(Self { models })
    }

    /// Load pricing from cache or fetch from LiteLLM
    fn load_pricing(cache_dir: &Path) -> Result<HashMap<String, ModelPricing>> {
        let pricing_cache_path = cache_dir.join("pricing.json");

        // Check if cache exists and is fresh
        if let Ok(cache_file) = fs::read_to_string(&pricing_cache_path)
            && let Ok(cached) = serde_json::from_str::<PricingCache>(&cache_file)
        {
            let now = Utc::now().timestamp();
            let age = now - cached.timestamp;

            if age < Self::MAX_AGE_SECONDS {
                return Ok(cached.models);
            }
        }

        // Try to fetch fresh pricing
        match reqwest::blocking::get(Self::LITELLM_URL) {
            Ok(response) if response.status().is_success() => {
                let models: HashMap<String, ModelPricing> =
                    response.json().context("Failed to parse pricing JSON")?;

                // Cache the result
                let cache = PricingCache {
                    timestamp: Utc::now().timestamp(),
                    models: models.clone(),
                };

                if let Ok(cache_json) = serde_json::to_string_pretty(&cache) {
                    let _ = fs::write(&pricing_cache_path, cache_json);
                }

                Ok(models)
            }
            _ => {
                // Network error or bad response, try to use stale cache
                if let Ok(cache_file) = fs::read_to_string(&pricing_cache_path)
                    && let Ok(cached) = serde_json::from_str::<PricingCache>(&cache_file)
                {
                    return Ok(cached.models);
                }
                anyhow::bail!("Failed to fetch pricing and no cache available")
            }
        }
    }

    /// Get pricing for a specific model
    fn get_model_pricing(&self, model_name: &str) -> Option<&ModelPricing> {
        // Try exact match first
        if let Some(pricing) = self.models.get(model_name) {
            return Some(pricing);
        }

        // Try with common prefixes
        let prefixes = ["anthropic/", "claude-", "openai/"];
        for prefix in &prefixes {
            let candidate = format!("{}{}", prefix, model_name);
            if let Some(pricing) = self.models.get(&candidate) {
                return Some(pricing);
            }
        }

        // Try case-insensitive match
        let model_lower = model_name.to_lowercase();
        for (key, pricing) in &self.models {
            if key.to_lowercase() == model_lower {
                return Some(pricing);
            }
        }

        None
    }

    /// Calculate cost for a usage entry
    fn calculate_entry_cost(&self, entry: &UsageData) -> f64 {
        if let Some(model_name) = &entry.message.model
            && let Some(pricing) = self.get_model_pricing(model_name)
        {
            return pricing.calculate_cost(&entry.message.usage);
        }
        // Fallback to hardcoded estimate if model not found
        estimate_cost_fallback(entry)
    }
}

/// Semaphore cache for fast statusline rendering
#[derive(Debug, Serialize, Deserialize)]
struct Semaphore {
    date: String,
    last_output: String,
    last_update_time: u64,
    transcript_path: String,
    transcript_mtime: u64,
}

/// 5-hour billing block
#[derive(Debug)]
struct Block {
    start_time: DateTime<Utc>,
    end_time: DateTime<Utc>,
    cost_usd: f64,
    total_tokens: u64,
    is_active: bool,
}

/// Burn rate information
#[derive(Debug)]
struct BurnRate {
    cost_per_hour: f64,
    tokens_per_minute: u64,
}

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

/// Get cache directory from XDG_RUNTIME_DIR
fn get_cache_dir() -> Result<PathBuf> {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| format!("/run/user/{}", unsafe { libc::getuid() }));
    Ok(PathBuf::from(runtime_dir).join("ccusage-statusline-rs"))
}

/// Try to get cached output if valid
fn try_get_cached(cache_path: &Path, transcript_path: &str) -> Result<Option<String>> {
    if !cache_path.exists() {
        return Ok(None);
    }

    let mut file = match File::open(cache_path) {
        Ok(f) => f,
        Err(_) => return Ok(None),
    };

    // Try to acquire shared lock (non-blocking)
    if file.try_lock_shared().is_err() {
        return Ok(None);
    }

    let mut contents = String::new();
    file.read_to_string(&mut contents)?;

    let semaphore: Semaphore = match serde_json::from_str(&contents) {
        Ok(s) => s,
        Err(_) => return Ok(None),
    };

    // Check if cache is still valid
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();

    // Cache expires after 30 seconds (default refresh interval)
    let is_expired = now - semaphore.last_update_time >= 30;

    // Check if transcript file was modified
    let current_mtime = get_file_mtime(transcript_path)?;
    let is_file_modified = current_mtime != semaphore.transcript_mtime;

    if is_expired || is_file_modified {
        return Ok(None);
    }

    Ok(Some(semaphore.last_output))
}

/// Update cache with new output
fn update_cache(cache_path: &Path, transcript_path: &str, output: &str) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(cache_path)?;

    // Acquire exclusive lock
    file.lock_exclusive()?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();

    let mtime = get_file_mtime(transcript_path)?;

    let semaphore = Semaphore {
        date: Utc::now().to_rfc3339(),
        last_output: output.to_string(),
        last_update_time: now,
        transcript_path: transcript_path.to_string(),
        transcript_mtime: mtime,
    };

    let json = serde_json::to_string(&semaphore)?;
    file.write_all(json.as_bytes())?;

    file.unlock()?;
    Ok(())
}

/// Get file modification time in seconds
fn get_file_mtime(path: &str) -> Result<u64> {
    let metadata = fs::metadata(path)?;
    let mtime = metadata
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    Ok(mtime)
}

/// Generate statusline output
fn generate_statusline(hook_data: &HookData) -> Result<String> {
    // Get cache directory for pricing data
    let cache_dir = get_cache_dir()?;
    fs::create_dir_all(&cache_dir).context("Failed to create cache directory")?;

    // Initialize pricing fetcher (loads or fetches LiteLLM pricing)
    let pricing = PricingFetcher::new(cache_dir)?;

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

/// Find active billing block
fn find_active_block(claude_paths: &[PathBuf], pricing: &PricingFetcher) -> Result<Block> {
    let mut all_entries = Vec::new();
    let mut processed_hashes: HashSet<String> = HashSet::new();

    // Collect all usage data from all projects
    for base_path in claude_paths {
        for project_entry in fs::read_dir(base_path)? {
            let project_dir = project_entry?.path();
            if !project_dir.is_dir() {
                continue;
            }

            for session_entry in fs::read_dir(&project_dir)? {
                let session_file = session_entry?.path();
                if session_file.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                    continue;
                }

                // Read JSONL file
                let file = File::open(&session_file)?;
                let reader = BufReader::new(file);

                for line in reader.lines() {
                    let line = line?;
                    if line.trim().is_empty() {
                        continue;
                    }
                    if let Ok(entry) = serde_json::from_str::<UsageData>(&line) {
                        // Create unique hash from message_id and request_id (matching TypeScript logic)
                        let unique_hash = match (&entry.message.id, &entry.request_id) {
                            (Some(msg_id), Some(req_id)) => Some(format!("{}:{}", msg_id, req_id)),
                            _ => None,
                        };

                        // Skip duplicates (matching TypeScript deduplication)
                        if let Some(hash) = &unique_hash {
                            if processed_hashes.contains(hash) {
                                continue; // Skip duplicate entry
                            }
                            processed_hashes.insert(hash.clone());
                        }

                        all_entries.push(entry);
                    }
                }
            }
        }
    }

    // Sort by timestamp
    all_entries.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

    // Group into 5-hour blocks
    let blocks = group_into_blocks(&all_entries, pricing)?;

    // Find active block
    let now = Utc::now();
    for block in blocks.iter().rev() {
        if block.is_active && block.end_time > now {
            return Ok(Block {
                start_time: block.start_time,
                end_time: block.end_time,
                cost_usd: block.cost_usd,
                total_tokens: block.total_tokens,
                is_active: true,
            });
        }
    }

    // No active block found, return empty
    Ok(Block {
        start_time: now,
        end_time: now + Duration::hours(5),
        cost_usd: 0.0,
        total_tokens: 0,
        is_active: false,
    })
}

/// Floor timestamp to the beginning of the hour in UTC
fn floor_to_hour(timestamp: DateTime<Utc>) -> DateTime<Utc> {
    timestamp
        .with_minute(0)
        .and_then(|dt| dt.with_second(0))
        .and_then(|dt| dt.with_nanosecond(0))
        .unwrap_or(timestamp)
}

/// Group usage entries into 5-hour blocks (matching TypeScript logic)
fn group_into_blocks(entries: &[UsageData], pricing: &PricingFetcher) -> Result<Vec<Block>> {
    if entries.is_empty() {
        return Ok(Vec::new());
    }

    let session_duration_ms = 5 * 60 * 60 * 1000; // 5 hours in milliseconds
    let mut blocks = Vec::new();
    let mut current_block_start: Option<DateTime<Utc>> = None;
    let mut current_block_entries: Vec<&UsageData> = Vec::new();
    let now = Utc::now();

    for entry in entries {
        let entry_time = DateTime::parse_from_rfc3339(&entry.timestamp)?;
        let entry_time = entry_time.with_timezone(&Utc);

        match current_block_start {
            None => {
                // First entry - floor to hour
                current_block_start = Some(floor_to_hour(entry_time));
                current_block_entries = vec![entry];
            }
            Some(start) => {
                let time_since_block_start =
                    entry_time.timestamp_millis() - start.timestamp_millis();
                let last_entry = current_block_entries.last();

                let should_close_block = if let Some(last) = last_entry {
                    let last_time =
                        DateTime::parse_from_rfc3339(&last.timestamp)?.with_timezone(&Utc);
                    let time_since_last =
                        entry_time.timestamp_millis() - last_time.timestamp_millis();

                    time_since_block_start > session_duration_ms
                        || time_since_last > session_duration_ms
                } else {
                    false
                };

                if should_close_block {
                    // Close current block
                    let block = create_block_from_entries(
                        start,
                        &current_block_entries,
                        now,
                        session_duration_ms,
                        pricing,
                    );
                    blocks.push(block);

                    // Start new block (floored to hour)
                    current_block_start = Some(floor_to_hour(entry_time));
                    current_block_entries = vec![entry];
                } else {
                    // Add to current block
                    current_block_entries.push(entry);
                }
            }
        }
    }

    // Close the last block
    if let Some(start) = current_block_start
        && !current_block_entries.is_empty()
    {
        let block = create_block_from_entries(
            start,
            &current_block_entries,
            now,
            session_duration_ms,
            pricing,
        );
        blocks.push(block);
    }

    Ok(blocks)
}

/// Create a block from start time and entries (matching TypeScript logic)
fn create_block_from_entries(
    start_time: DateTime<Utc>,
    entries: &[&UsageData],
    now: DateTime<Utc>,
    session_duration_ms: i64,
    pricing: &PricingFetcher,
) -> Block {
    let end_time = start_time + Duration::milliseconds(session_duration_ms);

    // Find actual end time (last entry timestamp)
    let actual_end_time = entries
        .last()
        .and_then(|e| DateTime::parse_from_rfc3339(&e.timestamp).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or(start_time);

    // TypeScript logic: isActive = now - actualEndTime < sessionDuration && now < endTime
    let time_since_last_activity = now.timestamp_millis() - actual_end_time.timestamp_millis();
    let is_active = time_since_last_activity < session_duration_ms && now < end_time;

    // Aggregate costs and tokens
    let mut cost_usd = 0.0;
    let mut total_tokens = 0u64;

    for entry in entries {
        cost_usd += pricing.calculate_entry_cost(entry);
        total_tokens += entry.message.usage.input_tokens + entry.message.usage.output_tokens;
    }

    Block {
        start_time,
        end_time,
        cost_usd,
        total_tokens,
        is_active,
    }
}

/// Fallback cost estimation with hardcoded prices (when model not found in LiteLLM)
fn estimate_cost_fallback(entry: &UsageData) -> f64 {
    let model = entry
        .message
        .model
        .as_deref()
        .unwrap_or("claude-sonnet-4-20250514");

    // LiteLLM pricing (per token, not per million)
    let (
        input_price,
        output_price,
        cache_write_price,
        cache_read_price,
        input_price_above_200k,
        output_price_above_200k,
        cache_write_price_above_200k,
        cache_read_price_above_200k,
    ) = match model {
        "claude-sonnet-4-20250514" => (
            3e-6,    // input
            15e-6,   // output
            3.75e-6, // cache_creation
            3e-7,    // cache_read
            6e-6,    // input_above_200k
            22.5e-6, // output_above_200k
            7.5e-6,  // cache_creation_above_200k
            6e-7,    // cache_read_above_200k
        ),
        "claude-sonnet-4-5-20250929" => (
            3e-6,    // input (no tiered pricing)
            15e-6,   // output
            3.75e-6, // cache_creation
            3e-7,    // cache_read
            3e-6,    // same as base (no tiered pricing)
            15e-6, 3.75e-6, 3e-7,
        ),
        "claude-opus-4-1-20250805" => (
            15e-6,    // input (5x more expensive)
            75e-6,    // output
            18.75e-6, // cache_creation
            1.5e-6,   // cache_read (5x more expensive)
            15e-6,    // no tiered pricing
            75e-6, 18.75e-6, 1.5e-6,
        ),
        _ => (3e-6, 15e-6, 3.75e-6, 3e-7, 6e-6, 22.5e-6, 7.5e-6, 6e-7), // default to Sonnet 4
    };

    // Helper for tiered cost calculation (200k threshold for Claude models)
    let calc_tiered = |tokens: u64, base_price: f64, tiered_price: f64| -> f64 {
        if tokens <= 200_000 {
            tokens as f64 * base_price
        } else {
            (200_000.0 * base_price) + ((tokens - 200_000) as f64 * tiered_price)
        }
    };

    let input_cost = calc_tiered(
        entry.message.usage.input_tokens,
        input_price,
        input_price_above_200k,
    );
    let output_cost = calc_tiered(
        entry.message.usage.output_tokens,
        output_price,
        output_price_above_200k,
    );
    let cache_write_cost = calc_tiered(
        entry.message.usage.cache_creation_input_tokens,
        cache_write_price,
        cache_write_price_above_200k,
    );
    let cache_read_cost = calc_tiered(
        entry.message.usage.cache_read_input_tokens,
        cache_read_price,
        cache_read_price_above_200k,
    );

    input_cost + output_cost + cache_write_cost + cache_read_cost
}

/// Calculate burn rate for a block
fn calculate_burn_rate(block: &Block) -> Result<BurnRate> {
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

#[derive(Debug)]
struct ContextInfo {
    tokens: u64,
    percentage: u32,
}

/// Format block information
fn format_block_info(block: &Block) -> String {
    if !block.is_active {
        return "No active block".to_string();
    }

    let now = Utc::now();
    let remaining = (block.end_time - now).num_minutes();
    let hours = remaining / 60;
    let mins = remaining % 60;

    format!("${:.2} block ({}h {}m left)", block.cost_usd, hours, mins)
}

/// Format burn rate with emoji indicator
fn format_burn_rate(burn_rate: &BurnRate) -> String {
    let emoji = if burn_rate.tokens_per_minute < 2000 {
        "🟢".green().to_string()
    } else if burn_rate.tokens_per_minute < 5000 {
        "⚠️".yellow().to_string()
    } else {
        "🚨".red().to_string()
    };

    format!("${:.2}/hr {}", burn_rate.cost_per_hour, emoji)
}

/// Format context information
fn format_context(context: &Option<ContextInfo>) -> String {
    match context {
        Some(info) => {
            let color = if info.percentage < 70 {
                info.percentage.to_string().green().to_string()
            } else if info.percentage < 90 {
                info.percentage.to_string().yellow().to_string()
            } else {
                info.percentage.to_string().red().to_string()
            };

            format!("{} ({}%)", format_number(info.tokens), color)
        }
        None => "N/A".to_string(),
    }
}

/// Format number with thousand separators
fn format_number(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result.chars().rev().collect()
}
