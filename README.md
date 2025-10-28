# ccusage-statusline-rs

Ultra-fast Rust implementation of Claude Code usage statusline.

## Description

High-performance statusline for Claude Code that displays real-time usage metrics, billing blocks, and burn rates. Written in Rust for sub-millisecond response times with intelligent caching.

## Features

- ⚡ **Ultra-fast performance** - 15x faster than Node.js implementation (8ms vs 120ms warm)
- 📊 **Live API integration** - Real-time 5-hour and 7-day utilization from claude.ai API
- 🖥️ **Interactive mode** - Works as standalone tool or piped statusline
- 🦊 **Firefox cookie extraction** - Automatic authentication using Firefox profile
- 💰 **Accurate cost tracking** - Fetches daily pricing from LiteLLM, supports tiered pricing
- 🔄 **Smart caching** - XDG_RUNTIME_DIR-based caching with 24-hour pricing cache
- 🎯 **5-hour block tracking** - Matches Claude's billing cycles exactly
- 🧮 **Deduplication** - Prevents double-counting duplicate JSONL entries
- 🔥 **Burn rate monitoring** - Real-time cost per hour with visual indicators

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