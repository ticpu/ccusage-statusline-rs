use crate::paths::home_dir;
use anyhow::Result;
use inquire::ui::{RenderConfig, Styled};
use inquire::{CustomType, MultiSelect, Select};
use serde::{Deserialize, Serialize};
use std::fmt;
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

const API_DEPENDENT_ELEMENTS: &[StatusElement] = &[
    StatusElement::TimeRemaining5h,
    StatusElement::TimeRemaining7d,
    StatusElement::BurnRate,
    StatusElement::BurnRateEta,
    StatusElement::ApiMetrics5h,
    StatusElement::ApiMetrics7d,
    StatusElement::ApiMetricsSonnet,
];

impl StatusElement {
    fn label(&self) -> &'static str {
        match self {
            Self::Model => "🤖 Model",
            Self::BlockCost => "💰 Block cost",
            Self::TimeRemaining5h => "🕑 Time remaining (5h)",
            Self::TimeRemaining7d => "📅 Time remaining (7d)",
            Self::BurnRate => "🔥 Burn rate",
            Self::BurnRateEta => "⏱ Coding time remaining",
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
                "Usage rate as % of limit. Color changes at warning/danger thresholds."
            }
            Self::BurnRateEta => {
                "Time remaining before hitting limit. Visible above burn rate show threshold."
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thresholds {
    #[serde(default = "default_burn_rate_show")]
    pub burn_rate_show: u32,
    #[serde(default = "default_burn_rate_warning")]
    pub burn_rate_warning: u32,
    #[serde(default = "default_burn_rate_danger")]
    pub burn_rate_danger: u32,
    #[serde(default = "default_context_warning")]
    pub context_warning: u32,
    #[serde(default = "default_context_danger")]
    pub context_danger: u32,
}

fn default_burn_rate_show() -> u32 {
    80
}
fn default_burn_rate_warning() -> u32 {
    80
}
fn default_burn_rate_danger() -> u32 {
    100
}
fn default_context_warning() -> u32 {
    50
}
fn default_context_danger() -> u32 {
    70
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            burn_rate_show: default_burn_rate_show(),
            burn_rate_warning: default_burn_rate_warning(),
            burn_rate_danger: default_burn_rate_danger(),
            context_warning: default_context_warning(),
            context_danger: default_context_danger(),
        }
    }
}

impl Thresholds {
    pub fn burn_rate_show_ratio(&self) -> f64 {
        self.burn_rate_show as f64 / 100.0
    }

    pub fn burn_rate_warning_ratio(&self) -> f64 {
        self.burn_rate_warning as f64 / 100.0
    }

    pub fn burn_rate_danger_ratio(&self) -> f64 {
        self.burn_rate_danger as f64 / 100.0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheSettings {
    #[serde(default = "default_output_cache_secs")]
    pub output_cache_secs: u64,
    #[serde(default = "default_api_refresh_secs")]
    pub api_refresh_secs: u64,
    #[serde(default = "default_api_max_backoff_secs")]
    pub api_max_backoff_secs: u64,
}

fn default_output_cache_secs() -> u64 {
    300
}
fn default_api_refresh_secs() -> u64 {
    300
}
fn default_api_max_backoff_secs() -> u64 {
    1800
}

impl Default for CacheSettings {
    fn default() -> Self {
        Self {
            output_cache_secs: default_output_cache_secs(),
            api_refresh_secs: default_api_refresh_secs(),
            api_max_backoff_secs: default_api_max_backoff_secs(),
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StatuslineConfig {
    pub enabled_elements: Vec<StatusElement>,
    #[serde(default)]
    pub thresholds: Thresholds,
    #[serde(default)]
    pub cache: CacheSettings,
    #[serde(default = "default_true")]
    pub show_emojis: bool,
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
            thresholds: Thresholds::default(),
            cache: CacheSettings::default(),
            show_emojis: true,
        }
    }
}

impl StatuslineConfig {
    pub fn needs_api(&self) -> bool {
        self.enabled_elements
            .iter()
            .any(|e| API_DEPENDENT_ELEMENTS.contains(e))
    }

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

enum MainMenu {
    Elements,
    Thresholds,
    Help,
    SaveAndExit,
}

impl fmt::Display for MainMenu {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Elements => write!(f, "Elements       Enable/disable statusline elements"),
            Self::Thresholds => {
                write!(
                    f,
                    "Thresholds     Configure visibility and color thresholds"
                )
            }
            Self::Help => write!(f, "Help           Show element descriptions"),
            Self::SaveAndExit => write!(f, "Save & exit"),
        }
    }
}

enum ThresholdMenu {
    BurnRateShow(u32),
    BurnRateWarning(u32),
    BurnRateDanger(u32),
    ContextWarning(u32),
    ContextDanger(u32),
    Back,
}

impl fmt::Display for ThresholdMenu {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BurnRateShow(v) => {
                write!(
                    f,
                    "Burn rate visibility    {v}%  (show burn rate above this)"
                )
            }
            Self::BurnRateWarning(v) => {
                write!(f, "Burn rate warning       {v}%  (yellow color threshold)")
            }
            Self::BurnRateDanger(v) => {
                write!(f, "Burn rate danger       {v}%  (red color threshold)")
            }
            Self::ContextWarning(v) => {
                write!(f, "Context warning         {v}%  (yellow color threshold)")
            }
            Self::ContextDanger(v) => {
                write!(f, "Context danger          {v}%  (red color threshold)")
            }
            Self::Back => write!(f, "Back"),
        }
    }
}

pub fn run_config_menu() -> Result<()> {
    inquire::set_global_render_config(
        RenderConfig::default_colored().with_canceled_prompt_indicator(Styled::new("")),
    );

    let mut config = StatuslineConfig::load().unwrap_or_default();

    loop {
        let menu = vec![
            MainMenu::Elements,
            MainMenu::Thresholds,
            MainMenu::Help,
            MainMenu::SaveAndExit,
        ];

        let Some(choice) = Select::new("Configure statusline:", menu).prompt_skippable()? else {
            break;
        };

        match choice {
            MainMenu::Elements => configure_elements(&mut config)?,
            MainMenu::Thresholds => configure_thresholds(&mut config.thresholds)?,
            MainMenu::Help => print_help(),
            MainMenu::SaveAndExit => {
                config.save()?;
                println!(
                    "\nConfiguration saved to {}",
                    StatuslineConfig::config_path()?.display()
                );
                println!("  Emojis: {}", if config.show_emojis { "on" } else { "off" });
                break;
            }
        }
    }

    Ok(())
}

fn configure_elements(config: &mut StatuslineConfig) -> Result<()> {
    let all_elements = StatusElement::all();
    let emojis_label = "😀 Emojis";
    let mut options: Vec<String> = all_elements
        .iter()
        .map(|e| e.label().to_string())
        .collect();
    options.push(emojis_label.to_string());

    let mut default_indices: Vec<usize> = all_elements
        .iter()
        .enumerate()
        .filter(|(_, elem)| config.enabled_elements.contains(elem))
        .map(|(i, _)| i)
        .collect();
    if config.show_emojis {
        default_indices.push(options.len() - 1);
    }

    let Some(selected) = MultiSelect::new("Select elements to display:", options)
        .with_default(&default_indices)
        .with_page_size(16)
        .prompt_skippable()?
    else {
        return Ok(());
    };

    config.show_emojis = selected.iter().any(|label| label == emojis_label);
    config.enabled_elements = selected
        .iter()
        .filter_map(|label| all_elements.iter().find(|e| e.label() == label).cloned())
        .collect();

    Ok(())
}

fn configure_thresholds(thresholds: &mut Thresholds) -> Result<()> {
    loop {
        let menu = vec![
            ThresholdMenu::BurnRateShow(thresholds.burn_rate_show),
            ThresholdMenu::BurnRateWarning(thresholds.burn_rate_warning),
            ThresholdMenu::BurnRateDanger(thresholds.burn_rate_danger),
            ThresholdMenu::ContextWarning(thresholds.context_warning),
            ThresholdMenu::ContextDanger(thresholds.context_danger),
            ThresholdMenu::Back,
        ];

        let Some(choice) = Select::new("Thresholds:", menu).prompt_skippable()? else {
            break;
        };

        match choice {
            ThresholdMenu::BurnRateShow(_) => {
                if let Some(v) =
                    prompt_threshold("Burn rate visibility % (0-100):", thresholds.burn_rate_show)?
                {
                    thresholds.burn_rate_show = v;
                }
            }
            ThresholdMenu::BurnRateWarning(_) => {
                if let Some(v) =
                    prompt_threshold("Burn rate warning % (0-100):", thresholds.burn_rate_warning)?
                {
                    thresholds.burn_rate_warning = v;
                }
            }
            ThresholdMenu::BurnRateDanger(_) => {
                if let Some(v) =
                    prompt_threshold("Burn rate danger % (0-200):", thresholds.burn_rate_danger)?
                {
                    thresholds.burn_rate_danger = v;
                }
            }
            ThresholdMenu::ContextWarning(_) => {
                if let Some(v) =
                    prompt_threshold("Context warning % (0-100):", thresholds.context_warning)?
                {
                    thresholds.context_warning = v;
                }
            }
            ThresholdMenu::ContextDanger(_) => {
                if let Some(v) =
                    prompt_threshold("Context danger % (0-100):", thresholds.context_danger)?
                {
                    thresholds.context_danger = v;
                }
            }
            ThresholdMenu::Back => break,
        }
    }

    Ok(())
}

fn prompt_threshold(message: &str, current: u32) -> Result<Option<u32>> {
    CustomType::<u32>::new(message)
        .with_default(current)
        .with_error_message("Enter a number between 0 and 200")
        .with_validator(|val: &u32| {
            if *val <= 200 {
                Ok(inquire::validator::Validation::Valid)
            } else {
                Ok(inquire::validator::Validation::Invalid(
                    "Must be between 0 and 200".into(),
                ))
            }
        })
        .prompt_skippable()
        .map_err(Into::into)
}

fn print_help() {
    println!();
    for elem in StatusElement::all() {
        println!("  {}  {}", elem.label(), elem.description());
    }
    println!();
}
