//! Optional progress reporting for long conversions (large/heavy files).
//!
//! Conversions are otherwise opaque: a 350 MB PDF looks identical to a hung
//! process. A caller (CLI `--verbose`, the MCP server's logs, the desktop app)
//! can install a [`ProgressCallback`] on [`crate::ConvertOptions`] to receive
//! phase + percentage updates. It is entirely optional — no callback means no
//! overhead and the original (fast) code paths are used.

use std::sync::Arc;

/// A single progress update.
#[derive(Debug, Clone)]
pub struct Progress {
    /// Coarse phase id: `"detect"`, `"convert"`, `"pdf"`, `"python"`, `"done"`.
    pub phase: &'static str,
    /// Human-readable detail.
    pub message: String,
    /// Units completed so far (e.g. pages), when known.
    pub current: Option<u64>,
    /// Total units, when known — together with `current` this yields a %.
    pub total: Option<u64>,
}

impl Progress {
    pub fn msg(phase: &'static str, message: impl Into<String>) -> Self {
        Progress { phase, message: message.into(), current: None, total: None }
    }
    pub fn step(phase: &'static str, message: impl Into<String>, current: u64, total: u64) -> Self {
        Progress { phase, message: message.into(), current: Some(current), total: Some(total) }
    }
    /// Percentage 0–100 when both counts are present.
    pub fn percent(&self) -> Option<u8> {
        match (self.current, self.total) {
            (Some(c), Some(t)) if t > 0 => Some(((c.min(t) * 100) / t) as u8),
            _ => None,
        }
    }
}

/// A cloneable progress sink. Wraps an `Arc<dyn Fn>` so [`crate::ConvertOptions`]
/// stays `Clone` (used by the recursive ZIP converter) while still having a
/// hand-written `Debug` (the closure isn't `Debug`).
#[derive(Clone)]
pub struct ProgressCallback(pub Arc<dyn Fn(Progress) + Send + Sync>);

impl ProgressCallback {
    pub fn new(f: impl Fn(Progress) + Send + Sync + 'static) -> Self {
        ProgressCallback(Arc::new(f))
    }
    pub fn report(&self, p: Progress) {
        (self.0)(p);
    }
}

impl std::fmt::Debug for ProgressCallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProgressCallback(..)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_computation() {
        assert_eq!(Progress::step("pdf", "", 45, 90).percent(), Some(50));
        assert_eq!(Progress::step("pdf", "", 90, 90).percent(), Some(100));
        assert_eq!(Progress::step("pdf", "", 5, 0).percent(), None);
        assert_eq!(Progress::msg("convert", "x").percent(), None);
        // current clamped to total so we never report >100%.
        assert_eq!(Progress::step("pdf", "", 200, 100).percent(), Some(100));
    }
}
