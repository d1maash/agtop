# agtop

`htop` for your local AI coding agents. `agtop` scans Claude Code (`~/.claude/projects`) and Codex (`~/.codex/sessions`) session logs and shows a live TUI with token usage, model, project, and activity for every session on your machine.

![demo placeholder]

## Install

### Homebrew (macOS / Linux)

```bash
brew install d1maash/tap/agtop
```

### Shell installer (macOS / Linux)

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/d1maash/agtop/releases/latest/download/agtop-installer.sh | sh
```

### From source

```bash
cargo install --git https://github.com/d1maash/agtop
```

## Usage

```bash
agtop          # live TUI
agtop --once   # one-shot table dump
```

### Keys

| Key       | Action                  |
| --------- | ----------------------- |
| `q` / Esc | Quit                    |
| `↑` / `k` | Move selection up       |
| `↓` / `j` | Move selection down     |
| `t`       | Sort by total tokens    |
| `a`       | Sort by last activity   |
| `p`       | Sort by project         |
| `A`       | Toggle inactive sessions|
| `r`       | Force refresh           |

## What it shows

- **SRC** — `claude` or `codex`
- **ID** — first 8 chars of the session id
- **PROJECT** — last segment of the session's working directory
- **MODEL** — model used in the session (e.g. `claude-opus-4-7`, `gpt-5.5`)
- **IN / OUT / CACHE / TOTAL** — token counters aggregated from the session log
- **TURNS** — number of assistant turns (Claude) / token-count events (Codex)
- **AGO** — time since last activity
- **STATUS** — `active` if last activity is within 2 minutes

## How it works

`agtop` reads append-only JSONL logs that Claude Code and Codex write locally. No network calls, no API keys, no daemon — just file reads.

## License

MIT
