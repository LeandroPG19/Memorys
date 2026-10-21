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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_that_worked_gives_its_value_and_records_nothing() {
        let mut lost = LostSignals::default();
        let got: Vec<u8> = lost.note("working_memory", Ok::<_, String>(vec![1, 2, 3]));
        assert_eq!(
            got,
            [1, 2, 3],
            "a read that worked must give its own rows, not the default"
        );
        assert!(
            lost.names().is_empty(),
            "a read that worked lost nothing: a healthy answer carries no degradation keys"
        );
    }

    #[test]
    fn a_read_that_failed_gives_the_default_and_names_what_was_lost_once() {
        let mut lost = LostSignals::default();
        let rows: Vec<u8> = lost.note("working_memory", Err::<Vec<u8>, _>("table is gone"));
        let again: Option<u8> = lost.note("working_memory", Err::<Option<u8>, _>("still gone"));
        let count: i64 = lost.note("session_goals", Err::<i64, _>("column is gone"));

        assert!(rows.is_empty() && again.is_none() && count == 0);
        assert_eq!(
            lost.names(),
            ["working_memory", "session_goals"],
            "each lost signal is named once, in the order it was lost: the response lists them \
             for a program to match and a repeat would only be noise"
        );
    }
}
