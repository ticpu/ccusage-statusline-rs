use crate::paths::claude_config_dir;
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

fn get_settings_path() -> Result<PathBuf> {
    Ok(claude_config_dir()?.join("settings.json"))
}

/// Prompt user for yes/no confirmation
fn prompt_yes_no(prompt: &str) -> Result<bool> {
    print!("{} [y/n]: ", prompt);
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;

    Ok(input
        .trim()
        .eq_ignore_ascii_case("y"))
}

/// Install statusLine configuration
pub fn install() -> Result<()> {
    let config_dir = claude_config_dir()?;
    if !config_dir.exists() {
        anyhow::bail!(
            "Config directory {} does not exist — run Claude Code once first",
            config_dir.display()
        );
    }

    let settings_path = config_dir.join("settings.json");

    let mut settings: Value = if settings_path.exists() {
        let content = fs::read_to_string(&settings_path).context("Failed to read settings file")?;
        serde_json::from_str(&content).context("Failed to parse settings.json (invalid JSON)")?
    } else {
        println!("Creating new settings file: {}", settings_path.display());
        json!({})
    };

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
    let raw = binary_path
        .to_str()
        .context("Binary path contains invalid UTF-8")?;
    // Claude Code invokes statusLine commands via Git Bash on Windows, so backslashes
    // in the path would be interpreted as escape characters. Use forward slashes instead.
    let binary_path_str: std::borrow::Cow<str> = if cfg!(windows) {
        raw.replace('\\', "/")
            .into()
    } else {
        raw.into()
    };

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
        println!("ℹ️  Settings file does not exist. Nothing to uninstall.");
        return Ok(());
    }

    // Read and parse settings
    let content = fs::read_to_string(&settings_path).context("Failed to read settings file")?;

    let mut settings: Value =
        serde_json::from_str(&content).context("Failed to parse settings.json (invalid JSON)")?;

    // Check if statusLine exists
    if settings
        .get("statusLine")
        .is_none()
    {
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
