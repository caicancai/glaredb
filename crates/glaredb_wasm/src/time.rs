use std::time::Duration;

use glaredb_core::runtime::time::RuntimeInstant;
use tracing::warn;

thread_local! {
    /// The global performance object.
    ///
    /// May be None if there's no global performance or window object (is this
    /// possible?).
    static GLOBAL_PERFORMANCE: Option<web_sys::Performance> = web_sys::window().and_then(|window| window.performance());
}

/// Instant implementation that uses the browser's performance api.
#[derive(Debug, Clone, PartialEq)]
pub struct PerformanceInstant(Duration);

impl RuntimeInstant for PerformanceInstant {
    fn now() -> Self {
        GLOBAL_PERFORMANCE.with(|maybe_perf| match maybe_perf.as_ref() {
            Some(perf) => {
                // Return an f64 representing millis since time origin.
                //
                // Go ahead and convert to equivalent duration.
                let millis = perf.now();
                let total_nanos = (millis * 1_000_000.0).round() as u128;
                let secs = (total_nanos / 1_000_000_000) as u64;
                let nanos = (total_nanos % 1_000_000_000) as u32;
                PerformanceInstant(Duration::new(secs, nanos))
            }
            None => {
                warn!("Missing performance object, returning zero");
                PerformanceInstant(Duration::new(0, 0))
            }
        })
    }

    fn duration_since(&self, earlier: Self) -> std::time::Duration {
        // Both durations created from same reference (time origin), we can just
        // subtract them.
        self.0.saturating_sub(earlier.0)
    }
}
