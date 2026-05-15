# agtop

`htop` for your local AI coding agents.

`agtop` watches Claude Code (`~/.claude/projects`) and Codex (`~/.codex/sessions`) session logs and shows a live TUI with token usage, **dollar cost**, **tokens in the last 60 seconds**, project, model, and activity for every session on your machine. No network calls, no API keys, no daemon — just local files.

```
┌────────────────────────────────────────────────────────────────────────────────────────┐
│ agtop   sessions: 24  active: 2  claude:21  codex:3   tokens: 412.8M   $9661.56   8.4k tok/60s │
└────────────────────────────────────────────────────────────────────────────────────────┘
┌ sessions (24) — sort: cost ───────────────────────────────────────────────────────────┐
│ SRC     ID         PROJECT             MODEL              TOTAL    TOK/60S     $   AGO  STATUS │
│ claude  77fdea4e   joinway-learn-ai    claude-opus-4-7   66.5M       4.1k  $124.32  3m   ● active │
│ claude  567a1738   PromptLab           claude-opus-4-7   32.3M          ·  $122.81  2h     idle  │
│ codex   019e1d9b   joinway-learn-ai    gpt-5.5            8.5M          ·    $0.86  1d     idle  │
│ ...                                                                                            │
└────────────────────────────────────────────────────────────────────────────────────────┘
 q  quit   ↑↓/jk  nav   t  tokens   c  cost   m  rate   a  activity   p  project   s  source   A  show:running
```

## Install

### Homebrew (macOS / Linux)

```bash
brew install d1maash/tap/agtop
```

Already installed? `brew upgrade agtop`.

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
agtop            # live TUI (refreshes as session logs grow)
agtop --once     # one-shot dump to stdout (good for scripts / cron)
agtop --version
```

### Keys

| Key       | Action                                        |
| --------- | --------------------------------------------- |
| `q` / Esc | Quit                                          |
| `↑` / `k` | Move selection up                             |
| `↓` / `j` | Move selection down                           |
| `t`       | Sort by total tokens                          |
| `c`       | Sort by cost ($)                              |
| `m`       | Sort by current rate (tokens in last 60s)     |
| `a`       | Sort by last activity                         |
| `p`       | Sort by project name                          |
| `s`       | Sort by source (claude / codex)               |
| `A`       | Toggle: show only running sessions vs. all    |

## Columns

| Column      | Meaning                                                                       |
| ----------- | ----------------------------------------------------------------------------- |
| `SRC`       | `claude` (Claude Code) or `codex` (Codex)                                     |
| `ID`        | First 8 chars of the session ID                                               |
| `PROJECT`   | Last segment of the session's working directory                               |
| `MODEL`     | Model used (e.g. `claude-opus-4-7`, `gpt-5.5`, `claude-sonnet-4-6`)           |
| `IN`        | Total input tokens                                                            |
| `OUT`       | Total output tokens                                                           |
| `CACHE`     | Cache read + cache write tokens                                               |
| `TOTAL`     | Sum of all token counters                                                     |
| `TOK/60S`   | Sum of token deltas in the last 60 wall-clock seconds (windowed count, not an instantaneous rate — a single 30k-token burst within the window reads as `30.0k`) |
| `$`         | Estimated cost in USD using the model's public list price                     |
| `AGO`       | Time since last activity                                                      |
| `STATUS`    | `● active` if last activity is within 2 minutes, otherwise `idle`             |

The header row also shows aggregate totals across all visible sessions: token count, total $, and the global last-60s token sum.

## How it works

1. **Initial scan.** On startup, `agtop` walks `~/.claude/projects/**/*.jsonl` and `~/.codex/sessions/**/rollout-*.jsonl`, parses every line, and builds an in-memory snapshot per session.
2. **Live tail via `notify`.** A filesystem watcher subscribes to changes under both roots. When a session log grows, `agtop` reads only the new bytes (from the saved offset), parses just the new lines, and updates that session's counters.
3. **Rate window.** Each new token-bearing event is recorded with its timestamp in a per-session sliding window. `TOK/60S` is the sum of token deltas observed in the last 60 seconds of wall-clock time — a windowed count, not an instantaneous rate.
4. **Cost.** Tokens are multiplied by the model's public list price (input / output / cache-read / cache-write all priced separately). The full table lives in `src/pricing.rs` — patch it if vendors change rates.

No telemetry, no API requests, no background daemon. The TUI does the work itself and exits clean when you quit.

## Supported sources

| Agent       | Log path                                  | Status |
| ----------- | ----------------------------------------- | ------ |
| Claude Code | `~/.claude/projects/**/*.jsonl`           | ✅     |
| Codex       | `~/.codex/sessions/**/rollout-*.jsonl`    | ✅     |

More planned (Cursor, Aider, Gemini CLI, Goose). PRs welcome.

### Platform support

`agtop` builds and runs everywhere Rust + a TTY work, but the "is this session
*currently* attached to a running CLI?" detection (used by the default `A`
filter) is **macOS / Linux only** — it shells out to `ps` and `lsof`. On
Windows the binary still works; it just falls back to an mtime heuristic
("modified in the last 2 minutes = running"), which is good enough in
practice but can show ghost sessions for ~2 minutes after a CLI exits.

## Pricing accuracy

The cost column uses **public list prices** as of `v0.2.0`. They do *not* account for:

- Anthropic / OpenAI plan discounts (Claude Pro, Codex Plus, ChatGPT Team, etc.)
- Enterprise contracts or volume rebates
- Vendor price changes after this version was cut

Treat `$` as a high-water-mark upper bound. If you're on a flat-rate subscription, you're paying less than what `agtop` reports.

To override prices for your situation, edit `src/pricing.rs` and rebuild — there's one `lookup()` function with model-name patterns.

## Roadmap

- [ ] Detail view (Enter on a row): recent tool calls, model swaps, file edits
- [ ] Search / filter (`/` like vim)
- [ ] More agents: Cursor, Aider, Gemini CLI, Goose
- [ ] `--json` / `--prom` exporters for Grafana
- [ ] macOS menubar widget showing live `tok/min`
- [ ] Daily / weekly report mode (`agtop report --since=7d`)

## Contributing

Issues and PRs welcome at https://github.com/d1maash/agtop. To run locally:

```bash
git clone https://github.com/d1maash/agtop
cd agtop
cargo run
cargo run -- --once   # data-layer smoke test
```

CI (`.github/workflows/ci.yml`) runs `cargo fmt --check`, `cargo clippy
--all-targets -- -D warnings`, and `cargo test --all-targets` on every push
and PR. Run those locally before opening a PR.

## License

MIT — see [LICENSE](LICENSE).
