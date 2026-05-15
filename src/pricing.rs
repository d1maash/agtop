// Pricing per 1M tokens, USD. Best-effort public list prices as of 2026-05.
// Tweak if vendors change rates. Returns None for unknown models so the UI
// shows "-" instead of misleading numbers.

#[derive(Debug, Clone, Copy)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

const ANTHROPIC_OPUS: Price = Price {
    input: 15.0,
    output: 75.0,
    cache_read: 1.5,
    cache_write: 18.75,
};

const ANTHROPIC_SONNET: Price = Price {
    input: 3.0,
    output: 15.0,
    cache_read: 0.3,
    cache_write: 3.75,
};

const ANTHROPIC_HAIKU: Price = Price {
    input: 1.0,
    output: 5.0,
    cache_read: 0.1,
    cache_write: 1.25,
};

const OPENAI_GPT5: Price = Price {
    input: 1.25,
    output: 10.0,
    cache_read: 0.125,
    cache_write: 0.0,
};

const OPENAI_O4_MINI: Price = Price {
    input: 1.1,
    output: 4.4,
    cache_read: 0.275,
    cache_write: 0.0,
};

pub fn lookup(model: &str) -> Option<Price> {
    let m = model.to_ascii_lowercase();
    if m.contains("opus") {
        Some(ANTHROPIC_OPUS)
    } else if m.contains("sonnet") {
        Some(ANTHROPIC_SONNET)
    } else if m.contains("haiku") {
        Some(ANTHROPIC_HAIKU)
    } else if m.starts_with("gpt-5") || m == "gpt-5" {
        Some(OPENAI_GPT5)
    } else if m.contains("o4-mini") || m.contains("o3-mini") {
        Some(OPENAI_O4_MINI)
    } else {
        None
    }
}

pub fn cost_usd(model: Option<&str>, t: &crate::model::TokenStats) -> Option<f64> {
    let model = model?;
    let p = lookup(model)?;
    let per = 1_000_000.0;
    // `t.reasoning` is informational only — for both Claude and Codex, the
    // vendor's `output_tokens` already includes reasoning/thinking tokens, so
    // billing it again here would double-charge.
    let cost = (t.input as f64) * p.input / per
        + (t.output as f64) * p.output / per
        + (t.cache_read as f64) * p.cache_read / per
        + (t.cache_creation as f64) * p.cache_write / per;
    Some(cost)
}
