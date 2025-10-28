use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

/// Get the path to Claude settings file
fn get_settings_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME environment variable not set")?;
    Ok(PathBuf::from(home).join(".claude").join("settings.json"))
}

/// Prompt user for yes/no confirmation
fn prompt_yes_no(prompt: &str) -> Result<bool> {
    print!("{} [y/n]: ", prompt);
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;

    Ok(input.trim().eq_ignore_ascii_case("y"))
}

/// Install statusLine configuration
pub fn install() -> Result<()> {
    let settings_path = get_settings_path()?;

    if !settings_path.exists() {
        bail!(
            "Settings file not found: {}\n\
            Please run Claude Code at least once first to create the settings file.",
            settings_path.display()
        );
    }

    // Read and parse settings
    let content = fs::read_to_string(&settings_path).context("Failed to read settings file")?;

    let mut settings: Value =
        serde_json::from_str(&content).context("Failed to parse settings.json (invalid JSON)")?;

    // Check if statusLine already exists
    if let Some(existing) = settings.get("statusLine") {
        println!("⚠️  statusLine is already configured:");
        println!("{}", serde_json::to_string_pretty(existing)?);
        println!();

        if !prompt_yes_no("Do you want to overwrite it?")? {
            println!("Installation cancelled.");
            return Ok(());
        }
    }

    // Get the current binary path
    let binary_path =
        std::env::current_exe().context("Failed to determine current executable path")?;
    let binary_path_str = binary_path
        .to_str()
        .context("Binary path contains invalid UTF-8")?;

    // Create statusLine configuration
    let status_line_config = json!({
        "type": "command",
        "command": binary_path_str
    });

    settings["statusLine"] = status_line_config;

    // Write back to file
    let updated_content = serde_json::to_string_pretty(&settings)?;
    fs::write(&settings_path, updated_content).context("Failed to write settings file")?;

    println!("✅ Successfully installed statusLine configuration!");
    println!("   Command: {}", binary_path_str);
    println!();
    println!("Restart Claude Code for changes to take effect.");

    Ok(())
}

/// Uninstall statusLine configuration
pub fn uninstall() -> Result<()> {
    let settings_path = get_settings_path()?;

    if !settings_path.exists() {
        bail!("Settings file not found: {}", settings_path.display());
    }

    // Read and parse settings
    let content = fs::read_to_string(&settings_path).context("Failed to read settings file")?;

    let mut settings: Value =
        serde_json::from_str(&content).context("Failed to parse settings.json (invalid JSON)")?;

    // Check if statusLine exists
    if settings.get("statusLine").is_none() {
        println!("ℹ️  statusLine is not configured. Nothing to uninstall.");
        return Ok(());
    }

    // Remove statusLine
    if let Some(obj) = settings.as_object_mut() {
        obj.remove("statusLine");
    }

    // Write back to file
    let updated_content = serde_json::to_string_pretty(&settings)?;
    fs::write(&settings_path, updated_content).context("Failed to write settings file")?;

    println!("✅ Successfully removed statusLine configuration!");
    println!();
    println!("Restart Claude Code for changes to take effect.");

    Ok(())
}
