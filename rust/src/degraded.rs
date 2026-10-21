//! Reads that failed while the thing they belong to went on.
//!
//! A handler that assembles an answer from several queries used to end each secondary one in
//! `unwrap_or_default()`. The answer was still produced, and a signal that could not be read looked
//! exactly like a signal that was read and found empty: no session boost, a verified fact shown as
//! merely asserted, `artifacts: []` for an index nobody could open. The 0.32 audit counted 60 such
//! reads in `src/`. `LostSignals` is the one place that logs the failure with the name of what was
//! lost and remembers it, so the handler can put that name in its response.

use std::fmt::Display;

#[derive(Default)]
pub struct LostSignals(Vec<&'static str>);

impl LostSignals {
    /// The value of a read, or its default after logging the failure under `signal` and recording it.
    /// A signal lost twice is named once.
    pub fn note<T: Default>(&mut self, signal: &'static str, read: Result<T, impl Display>) -> T {
        match read {
            Ok(value) => value,
            Err(why) => {
                tracing::warn!(signal, error = %why, "a read failed; the answer goes on without it");
                if !self.0.contains(&signal) {
                    self.0.push(signal);
                }
                T::default()
            }
        }
    }

    pub fn names(&self) -> &[&'static str] {
        &self.0
    }
}
