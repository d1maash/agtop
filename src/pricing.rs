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
    } else if m.starts_with("gpt-5") {
        Some(OPENAI_GPT5)
    } else if m.contains("o4-mini") || m.contains("o3-mini") {
        Some(OPENAI_O4_MINI)
    } else {
        None
    }
}

/// Context-window size (max input tokens) for a model, used to show how close
/// a session is to auto-compaction. Best-effort public values as of 2026-05;
/// `None` for unknown models so the UI shows no gauge rather than a wrong one.
/// Like pricing, these are plain constants — patch them if vendors change.
pub fn context_window(model: &str) -> Option<u64> {
    let m = model.to_ascii_lowercase();
    if m.contains("opus") || m.contains("sonnet") || m.contains("haiku") {
        Some(200_000)
    } else if m.starts_with("gpt-5") {
        Some(400_000)
    } else if m.contains("o4-mini") || m.contains("o3-mini") {
        Some(200_000)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_matches_known_families() {
        assert_eq!(lookup("claude-opus-4-7").map(|p| p.output), Some(75.0));
        assert_eq!(lookup("claude-sonnet-4-6").map(|p| p.output), Some(15.0));
        assert_eq!(lookup("claude-haiku-4-5").map(|p| p.output), Some(5.0));
        assert_eq!(lookup("gpt-5.5").map(|p| p.input), Some(1.25));
        assert_eq!(lookup("o4-mini").map(|p| p.input), Some(1.1));
        assert!(lookup("some-unknown-model").is_none());
    }

    #[test]
    fn context_window_matches_known_families() {
        assert_eq!(context_window("claude-opus-4-7"), Some(200_000));
        assert_eq!(context_window("gpt-5.5"), Some(400_000));
        assert_eq!(context_window("o3-mini"), Some(200_000));
        assert_eq!(context_window("mystery"), None);
    }
}
