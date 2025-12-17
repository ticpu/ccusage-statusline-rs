use anyhow::{Context, Result};
use inquire::{MultiSelect, Select};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum VersionChannel {
    #[default]
    Stable,
    Latest,
}

impl VersionChannel {
    fn label(&self) -> &'static str {
        match self {
            Self::Stable => "Stable (official releases)",
            Self::Latest => "Latest (npm registry)",
        }
    }

    fn all() -> Vec<Self> {
        vec![Self::Stable, Self::Latest]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum StatusElement {
    Model,
    BlockCost,
    TimeRemaining,
    BurnRate,
    Context,
    ApiMetrics,
    UpdateNotification,
    Directory,
}

impl StatusElement {
    fn label(&self) -> &'static str {
        match self {
            Self::Model => "🤖 Model",
            Self::BlockCost => "💰 Block cost",
            Self::TimeRemaining => "🕑 Time remaining",
            Self::BurnRate => "🔥 Burn rate",
            Self::Context => "🧠 Context",
            Self::ApiMetrics => "📊 API metrics",
            Self::UpdateNotification => "🔼 Update notification",
            Self::Directory => "📁 Directory",
        }
    }

    fn all() -> Vec<Self> {
        vec![
            Self::Model,
            Self::BlockCost,
            Self::TimeRemaining,
            Self::BurnRate,
            Self::Context,
            Self::ApiMetrics,
            Self::UpdateNotification,
            Self::Directory,
        ]
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StatuslineConfig {
    pub enabled_elements: Vec<StatusElement>,
    #[serde(default)]
    pub version_channel: VersionChannel,
}

impl Default for StatuslineConfig {
    fn default() -> Self {
        Self {
            enabled_elements: StatusElement::all(),
            version_channel: VersionChannel::default(),
        }
    }
}

impl StatuslineConfig {
    fn config_path() -> Result<PathBuf> {
        let home = std::env::var("HOME").context("HOME not set")?;
        Ok(PathBuf::from(home).join(".claude/ccusage-statusline-config.json"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;

        if !path.exists() {
            return Ok(Self::default());
        }

        let content = fs::read_to_string(&path)?;
        let config: Self = serde_json::from_str(&content)?;
        Ok(config)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::config_path()?;

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let content = serde_json::to_string_pretty(self)?;
        fs::write(&path, content)?;
        Ok(())
    }
}

pub fn run_config_menu() -> Result<()> {
    println!("Configure statusline\n");

    let current_config = StatuslineConfig::load().unwrap_or_default();

    // Status elements selection
    println!("Use ↑/↓ to navigate, Space to select/deselect, Enter to confirm\n");

    let all_elements = StatusElement::all();
    let options: Vec<String> = all_elements.iter().map(|e| e.label().to_string()).collect();

    let default_indices: Vec<usize> = all_elements
        .iter()
        .enumerate()
        .filter(|(_, elem)| current_config.enabled_elements.contains(elem))
        .map(|(i, _)| i)
        .collect();

    let selected = MultiSelect::new("Select elements to display:", options)
        .with_default(&default_indices)
        .with_page_size(20)
        .prompt()?;

    let enabled_elements: Vec<StatusElement> = selected
        .iter()
        .filter_map(|label| all_elements.iter().find(|e| e.label() == label).cloned())
        .collect();

    // Version channel selection
    println!();
    let all_channels = VersionChannel::all();
    let channel_options: Vec<String> = all_channels.iter().map(|c| c.label().to_string()).collect();

    let current_channel_idx = all_channels
        .iter()
        .position(|c| *c == current_config.version_channel)
        .unwrap_or(0);

    let selected_channel = Select::new("Update notification version channel:", channel_options)
        .with_starting_cursor(current_channel_idx)
        .prompt()?;

    let version_channel = all_channels
        .iter()
        .find(|c| c.label() == selected_channel)
        .copied()
        .unwrap_or_default();

    let new_config = StatuslineConfig {
        enabled_elements,
        version_channel,
    };
    new_config.save()?;

    println!(
        "\n✅ Configuration saved to {}",
        StatuslineConfig::config_path()?.display()
    );
    println!("\nEnabled elements:");
    for elem in &new_config.enabled_elements {
        println!("  {}", elem.label());
    }
    println!("\nVersion channel: {}", new_config.version_channel.label());

    Ok(())
}
