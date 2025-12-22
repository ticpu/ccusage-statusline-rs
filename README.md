# ccusage-statusline-rs

Ultra-fast Rust implementation of Claude Code usage statusline.

![Status Line Example](docs/images/status-line.png)

## Description

High-performance statusline for Claude Code that displays real-time usage metrics, billing blocks, and burn rates. Written in Rust for sub-millisecond response times with intelligent caching.

## Features

- **Ultra-fast performance** - 15x faster than Node.js implementation (8ms vs 120ms warm)
- **Live API integration** - Real-time 5-hour and 7-day utilization via Claude Code OAuth
- **Update notifications** - Checks for Claude Code updates with configurable channels (stable/latest)
- **Time remaining** - Shows time left in billing block with clock emoji easter egg
- **Interactive configuration** - Menu-based UI to enable/disable statusline elements
- **Interactive mode** - Works as standalone tool or piped statusline
- **OAuth authentication** - Uses Claude Code's native OAuth tokens from ~/.claude/.credentials.json
- **Accurate cost tracking** - Fetches daily pricing from LiteLLM, supports tiered pricing
- **Smart caching** - XDG_RUNTIME_DIR-based caching with 24-hour pricing cache
- **5-hour block tracking** - Matches Claude's billing cycles exactly
- **Deduplication** - Prevents double-counting duplicate JSONL entries
- **Burn rate monitoring** - Real-time cost per hour with visual indicators
- **Auto-install** - Creates ~/.claude/settings.json if missing during install

## Inspiration

This project is a Rust reimplementation of the statusline feature from [ccusage](https://github.com/ryoppippi/ccusage) by ryoppippi. The original TypeScript implementation provided the architecture and pricing logic that this Rust version optimizes for performance.

## Installation

### Quick Install

```bash
cargo build --release
sudo cp target/release/ccusage-statusline-rs /usr/local/bin/
ccusage-statusline-rs install
```

The `install` command will automatically configure `~/.claude/settings.json` for you.

### Manual Build

```bash
cargo build --release
```

The binary will be at `target/release/ccusage-statusline-rs`.

## Usage

### Automatic Configuration

After building, simply run:

```bash
ccusage-statusline-rs install
```

This will automatically add the statusLine configuration to `~/.claude/settings.json`. No manual editing, no bash dependencies required!

To remove the configuration:

```bash
ccusage-statusline-rs uninstall
```

### Customizing the Statusline

Configure which elements to display:

```bash
ccusage-statusline-rs config
```

![Configuration Menu](docs/images/config.png)

This opens an interactive menu where you can:
- Enable/disable individual elements (Model, Block cost, Time remaining, etc.)
- Choose update notification channel (stable/latest/off)
- Configuration persists in `~/.claude/ccusage-statusline-config.json`

### Manual Configuration (Not Recommended)

If you prefer to manually configure, add to your `~/.claude/settings.json`:

```json
{
  "statusLine": {
    "type": "command",
    "command": "/path/to/ccusage-statusline-rs"
  }
}
```

Replace `/path/to/` with the actual path to the binary.

## Performance

- **Rust**: ~8ms average (consistent across all runs)
- **Node.js warm**: ~120ms average (after JIT warmup)
- **Speedup**: 15x faster

## License

MIT - See LICENSE file for details.