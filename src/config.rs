use crate::paths::home_dir;
use anyhow::Result;
use inquire::MultiSelect;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum StatusElement {
    Model,
    BlockCost,
    TimeRemaining5h,
    TimeRemaining7d,
    BurnRate,
    BurnRateEta,
    Context,
    ApiMetrics5h,
    ApiMetrics7d,
    ApiMetricsSonnet,
    UpdateStable,
    UpdateLatest,
    Directory,
}

impl StatusElement {
    fn label(&self) -> &'static str {
        match self {
            Self::Model => "🤖 Model",
            Self::BlockCost => "💰 Block cost",
            Self::TimeRemaining5h => "🕑 Time remaining (5h)",
            Self::TimeRemaining7d => "📅 Time remaining (7d)",
            Self::BurnRate => "🔥 Burn rate (shows >80%)",
            Self::BurnRateEta => "⏱ Coding time remaining (shows >80%)",
            Self::Context => "🧠 Context",
            Self::ApiMetrics5h => "📊 API metrics (5h)",
            Self::ApiMetrics7d => "📊 API metrics (7d)",
            Self::ApiMetricsSonnet => "📊 API metrics (Sonnet 7d)",
            Self::UpdateStable => "🔼 Update (stable)",
            Self::UpdateLatest => "🔼 Update (latest)",
            Self::Directory => "📁 Directory",
        }
    }

    fn description(&self) -> &'static str {
        match self {
            Self::Model => "Currently active model name.",
            Self::BlockCost => "Estimated cost of the current 5-hour billing block.",
            Self::TimeRemaining5h => "Time until 5-hour billing block resets.",
            Self::TimeRemaining7d => "Time until 7-day billing window resets.",
            Self::BurnRate => {
                "Usage rate as % of limit. Colors: green (<80%), yellow (80-100%), red (>100%). Shows which limit (5h/7d) is critical."
            }
            Self::BurnRateEta => {
                "How much time you can continue coding at this rate. Shows time before hitting limit (red >100%) or time until reset (yellow 80-100%)."
            }
            Self::Context => "Current context window token usage and percentage.",
            Self::ApiMetrics5h => "5-hour API utilization percentage from Claude API.",
            Self::ApiMetrics7d => "7-day API utilization percentage from Claude API.",
            Self::ApiMetricsSonnet => "7-day Sonnet-specific utilization from Claude API.",
            Self::UpdateStable => {
                "Notification when a new stable Claude Code version is available."
            }
            Self::UpdateLatest => {
                "Notification when a new latest-channel Claude Code version is available."
            }
            Self::Directory => "Current working directory path.",
        }
    }

    fn all() -> Vec<Self> {
        vec![
            Self::Model,
            Self::BlockCost,
            Self::TimeRemaining5h,
            Self::TimeRemaining7d,
            Self::BurnRate,
            Self::BurnRateEta,
            Self::Context,
            Self::ApiMetrics5h,
            Self::ApiMetrics7d,
            Self::ApiMetricsSonnet,
            Self::UpdateStable,
            Self::UpdateLatest,
            Self::Directory,
        ]
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StatuslineConfig {
    pub enabled_elements: Vec<StatusElement>,
}

impl Default for StatuslineConfig {
    fn default() -> Self {
        Self {
            enabled_elements: vec![
                StatusElement::Model,
                StatusElement::BlockCost,
                StatusElement::TimeRemaining5h,
                StatusElement::BurnRate,
                StatusElement::Context,
                StatusElement::ApiMetrics5h,
                StatusElement::ApiMetrics7d,
                StatusElement::UpdateStable,
                StatusElement::Directory,
            ],
        }
    }
}

impl StatuslineConfig {
    fn config_path() -> Result<PathBuf> {
        Ok(home_dir()?.join(".claude/ccusage-statusline-config.json"))
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

    loop {
        let current_config = StatuslineConfig::load().unwrap_or_default();
        let all_elements = StatusElement::all();

        let mut options: Vec<String> = vec!["──── [?] View element descriptions ────".to_string()];
        options.extend(all_elements.iter().map(|e| e.label().to_string()));

        let default_indices: Vec<usize> = all_elements
            .iter()
            .enumerate()
            .filter(|(_, elem)| current_config.enabled_elements.contains(elem))
            .map(|(i, _)| i + 1)
            .collect();

        let selected = MultiSelect::new("Select elements to display:", options)
            .with_default(&default_indices)
            .with_page_size(15)
            .prompt()?;

        if selected.contains(&"──── [?] View element descriptions ────".to_string())
        {
            println!();
            for elem in StatusElement::all() {
                println!("  {}  {}", elem.label(), elem.description());
            }
            println!();
            continue;
        }

        let enabled_elements: Vec<StatusElement> = selected
            .iter()
            .filter_map(|label| all_elements.iter().find(|e| e.label() == label).cloned())
            .collect();

        let new_config = StatuslineConfig { enabled_elements };
        new_config.save()?;

        println!(
            "\nConfiguration saved to {}",
            StatuslineConfig::config_path()?.display()
        );
        println!("\nEnabled elements:");
        for elem in &new_config.enabled_elements {
            println!("  {}", elem.label());
        }

        break;
    }

    Ok(())
}
