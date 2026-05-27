# agtop

`htop` for your local AI coding agents.

`agtop` watches Claude Code (`~/.claude/projects`) and Codex (`~/.codex/sessions`) session logs and shows a live TUI with token usage, **dollar cost**, **tokens in the last 60 seconds**, **context-window fill**, project, model, and activity for every session on your machine. No network calls, no API keys, no daemon — just local files.

```
┌────────────────────────────────────────────────────────────────────────────────────────┐
│ agtop   sessions: 24  active: 2  claude:21  codex:3   tokens: 412.8M   $9661.56   8.4k tok/60s │
└────────────────────────────────────────────────────────────────────────────────────────┘
┌ sessions (24) — sort: cost ───────────────────────────────────────────────────────────┐
│ SRC     ID         PROJECT             MODEL              TOTAL   CTX   TOK/60S     $   AGO  STATUS │
│ claude  77fdea4e   joinway-learn-ai    claude-opus-4-7   66.5M   71%      4.1k  $124.32  3m   ● active │
│ claude  567a1738   PromptLab           claude-opus-4-7   32.3M   88%         ·  $122.81  2h     idle  │
│ codex   019e1d9b   joinway-learn-ai    gpt-5.5            8.5M   12%         ·    $0.86  1d     idle  │
│ ...                                                                                            │
└────────────────────────────────────────────────────────────────────────────────────────┘
 q quit  ↑↓/jk nav  ⏎ detail  g group  space pause  ? help   sort:cost ▼  show:running
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
agtop                       # live TUI (refreshes as session logs grow)
agtop --once                # one-shot table dump to stdout (good for scripts / cron)
agtop --json                # one-shot JSON dump (scripting, cron reports, Grafana)
agtop --running             # list JSONL files of currently-running CLI sessions
agtop report --since=7d     # offline usage report: by day, project, model, top sessions
agtop report --since=7d --json
agtop --version
```

### JSON output

`--json` prints a pretty-printed array, one object per session, sorted by most
recent activity. Fields are stable and safe to script against:

```jsonc
[
  {
    "source": "claude",            // "claude" or "codex"
    "id": "bf1f6afa-…",
    "project": "atop",
    "cwd": "/Users/me/code/atop",
    "file": "/Users/me/.claude/projects/…/bf1f6afa-….jsonl",
    "model": "claude-opus-4-7",
    "input": 46416,
    "output": 129686,
    "cache_read": 8626924,
    "cache_creation": 326402,
    "total": 9129428,
    "tokens_last_60s": 0,
    "cost_usd": 29.4831,           // null when the model price is unknown
    "context_used": 108731,        // last turn's prompt tokens
    "context_max": 200000,         // model context window, null if unknown
    "context_pct": 0.5436,         // context_used / context_max, null if unknown
    "turn_count": 119,
    "started_at": "2026-05-24T15:50:02.318+00:00",  // RFC 3339, null if unseen
    "last_activity": "2026-05-24T15:56:58.238+00:00"
  }
]
```

Pipe it anywhere, e.g. `agtop --json | jq '[.[] | select(.context_pct > 0.8)]'`
to find sessions approaching auto-compaction.

### Keys

Press `?` inside the TUI for the same reference. Pressing any sort key a second time **reverses** the order; the title bar shows the active direction (`▼` / `▲`).

| Key       | Action                                                          |
| --------- | --------------------------------------------------------------- |
| `q` / Esc | Quit                                                            |
| `?`       | Toggle the full key-reference overlay                           |
| `↑` / `k` | Move selection up                                               |
| `↓` / `j` | Move selection down                                             |
| `Enter`   | Open detail for the selected session, or collapse/expand a group |
| `t`       | Sort by total tokens (repeat to reverse)                        |
| `c`       | Sort by cost ($) (repeat to reverse)                            |
| `m`       | Sort by current rate (tokens in last 60s) (repeat to reverse)   |
| `a`       | Sort by last activity (repeat to reverse)                       |
| `p`       | Sort by project name (repeat to reverse)                        |
| `s`       | Sort by source (claude / codex) (repeat to reverse)             |
| `g`       | Group by project (tree view with per-project subtotals)         |
| `Space`   | Pause / resume the display (freezes fast-moving numbers)        |
| `A`       | Toggle: show only running sessions vs. all                      |

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
| `CTX`       | Last turn's prompt tokens as a percentage of the model's context window (green < 70%, yellow ≥ 70%, red ≥ 90%); `·` when the window size is unknown |
| `TOK/60S`   | Sum of token deltas in the last 60 wall-clock seconds (windowed count, not an instantaneous rate — a single 30k-token burst within the window reads as `30.0k`) |
| `$`         | Estimated cost in USD using the model's public list price                     |
| `AGO`       | Time since last activity                                                      |
| `STATUS`    | `● active` if last activity is within 2 minutes, otherwise `idle`             |

The header row also shows aggregate totals across all visible sessions: token count, total $, and the global last-60s token sum. On narrow terminals the table sheds optional columns automatically — `CACHE`, then `IN`/`OUT`, then `ID`/`MODEL`/`TOK/60S`/`CTX` — keeping `SRC`, `PROJECT`, `TOTAL`, `$`, `AGO`, and `STATUS` visible at all sizes. The header line trims its lowest-priority segments the same way.

### Grouping (tree view)

Press `g` to switch into the tree view. Sessions are grouped by project, with a subtotal header row per project showing the total tokens and cost across its sessions. Press `Enter` on a header row to collapse or expand that group; press `g` again to leave grouping. The active mode is shown in the table title (`, grouped`) and footer.

### Pause

Press `Space` to freeze the display — useful for reading or screenshotting fast-moving numbers. A yellow `[PAUSED]` chip appears in the footer; press `Space` again to resume. The watcher keeps parsing in the background; only the displayed snapshot is held.

## Detail view

Press `Enter` on a row to open a modal for that session. It shows a sparkline
of token activity over the last ~5 minutes (bucketed from the same per-event
samples that feed `TOK/60S`), the full in / out / cache-read / cache-write
breakdown, turn count, cost, the context-window gauge (`used / max (pct)`,
color-coded), model, file path, and start/last-activity times. `Esc` or `Enter`
closes it.

## Report

`agtop report` is an offline summary that reuses the same parsers (and the same `pricing.rs` table) as the live TUI, so the totals match. It walks every known JSONL once, aggregates, and prints:

```bash
agtop report                  # all history
agtop report --since=7d       # last 7 days
agtop report --since=24h --json
```

`--since` accepts `s` / `m` / `h` / `d` / `w` (e.g. `90s`, `30m`, `24h`, `7d`, `2w`). A session is included when its **last-activity** timestamp falls inside the window — whole sessions, not per-event splitting, so a session that crosses midnight counts in the day it last wrote.

The text output has four sections: totals, by-day (chronological), by-project and by-model (sorted by cost), and the top 10 sessions by cost. `--json` emits the same data as a structured object with stable field names.

## How it works

1. **Initial scan.** On startup, `agtop` walks `~/.claude/projects/**/*.jsonl` and `~/.codex/sessions/**/rollout-*.jsonl`, parses every line, and builds an in-memory snapshot per session.
2. **Live tail via `notify`.** A filesystem watcher subscribes to changes under both roots. When a session log grows, `agtop` reads only the new bytes (from the saved offset), parses just the new lines, and updates that session's counters.
3. **Rate window.** Each new token-bearing event is recorded with its timestamp in a per-session sliding window. `TOK/60S` is the sum of token deltas observed in the last 60 seconds of wall-clock time — a windowed count, not an instantaneous rate.
4. **Cost.** Tokens are multiplied by the model's public list price (input / output / cache-read / cache-write all priced separately). The full table lives in `src/pricing.rs` — patch it if vendors change rates.
5. **Context window.** The `CTX` column and detail-view gauge track the *last turn's* prompt size (fresh input + cached + cache-creation), not the lifetime sum, and divide it by the model's context window. This is the number that drives auto-compaction, so it tells you which session is about to hit its limit. Window sizes live next to prices in `src/pricing.rs`.

No telemetry, no API requests, no background daemon. The TUI does the work itself and exits clean when you quit. All filesystem stats and `ps`/`lsof` probes run on background threads, so the render loop never blocks on I/O.

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

## Pricing & context-window accuracy

The cost column uses **public list prices** as of `v0.2.0`. They do *not* account for:

- Anthropic / OpenAI plan discounts (Claude Pro, Codex Plus, ChatGPT Team, etc.)
- Enterprise contracts or volume rebates
- Vendor price changes after this version was cut

Treat `$` as a high-water-mark upper bound. If you're on a flat-rate subscription, you're paying less than what `agtop` reports.

The `CTX` percentage uses **best-effort context-window sizes** keyed off the model
name (e.g. 200k for Claude, 400k for GPT-5). Two caveats: extended-context
variants that aren't distinguishable by name (such as the 1M-token Opus/Sonnet
beta) are measured against the standard window, and unknown models show `·`
instead of a guess.

Both tables live in `src/pricing.rs`: `lookup()` for prices and
`context_window()` for window sizes, each one `match` over model-name patterns.
Edit and rebuild to fit your situation.

## Roadmap

- [x] Detail view (Enter on a row): token sparkline, in/out/cache breakdown, context-window gauge, model, path, cost
- [x] Context-window fill gauge (`CTX` column + detail view)
- [x] `--json` exporter for scripting / Grafana
- [x] Group-by-project tree view (`g`) with per-project subtotals
- [x] Adaptive layout: drops CACHE/IN/OUT/etc. on narrow terminals
- [x] Help overlay (`?`), pause (`Space`), reverse sort (repeat sort key)
- [x] Daily / weekly report mode (`agtop report --since=7d`)
- [ ] `--prom` (Prometheus) exporter
- [ ] Detail view extras: recent tool calls, model swaps, file edits
- [ ] Search / filter (`/` like vim)
- [ ] More agents: Cursor, Aider, Gemini CLI, Goose
- [ ] macOS menubar widget showing live `tok/min`

## Contributing

Issues and PRs welcome at https://github.com/d1maash/agtop. To run locally:

```bash
git clone https://github.com/d1maash/agtop
cd agtop
cargo run
cargo run -- --once   # data-layer smoke test (table)
cargo run -- --json   # data-layer smoke test (JSON)
```

CI (`.github/workflows/ci.yml`) runs `cargo fmt --check`, `cargo clippy
--all-targets -- -D warnings`, and `cargo test --all-targets` on every push
and PR. Run those locally before opening a PR.

## License

MIT — see [LICENSE](LICENSE).
