//! CRDT helpers: Lamport-style (actor, counter) comparisons and merge policy.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clock {
    pub actor: String,
    pub counter: i64,
}

impl Clock {
    pub fn dominates(&self, other: &Clock) -> bool {
        self.actor == other.actor && self.counter >= other.counter
            || self.counter > other.counter
                && (self.actor != other.actor || self.counter > other.counter)
    }

    /// Total order for LWW: higher counter wins; tie → lexicographic actor.
    pub fn happens_before_or_eq(&self, other: &Clock) -> bool {
        match self.counter.cmp(&other.counter) {
            std::cmp::Ordering::Less => true,
            std::cmp::Ordering::Greater => false,
            std::cmp::Ordering::Equal => self.actor <= other.actor,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergePick {
    KeepLocal,
    TakeIncoming,
    Concurrent,
}

pub fn pick_lww(local: &Clock, incoming: &Clock) -> MergePick {
    if local.actor == incoming.actor {
        return if incoming.counter > local.counter {
            MergePick::TakeIncoming
        } else {
            MergePick::KeepLocal
        };
    }
    match incoming.counter.cmp(&local.counter) {
        std::cmp::Ordering::Greater => MergePick::TakeIncoming,
        std::cmp::Ordering::Less => MergePick::KeepLocal,
        std::cmp::Ordering::Equal => {
            if incoming.actor > local.actor {
                MergePick::TakeIncoming
            } else if incoming.actor < local.actor {
                MergePick::KeepLocal
            } else {
                MergePick::Concurrent
            }
        }
    }
}

/// Concurrent same-counter different actors → keep both (caller stores previous_versions).
pub fn pick_observation_content(local: &Clock, incoming: &Clock) -> MergePick {
    let p = pick_lww(local, incoming);
    if p == MergePick::KeepLocal
        && local.counter == incoming.counter
        && local.actor != incoming.actor
    {
        return MergePick::Concurrent;
    }
    if local.counter == incoming.counter && local.actor != incoming.actor {
        return MergePick::Concurrent;
    }
    p
}

pub fn clock_from_row(actor: Option<&str>, counter: Option<i64>, fallback_actor: &str) -> Clock {
    Clock {
        actor: actor
            .filter(|s| !s.is_empty())
            .unwrap_or(fallback_actor)
            .to_string(),
        counter: counter.unwrap_or(0),
    }
}

/// Line-based OT: apply a single find→replace if `find` is unique; else return conflict detail.
pub fn ot_line_patch(base: &str, find: &str, replace: &str) -> Result<String, OtConflict> {
    let matches = base.matches(find).count();
    match matches {
        0 => Err(OtConflict {
            reason: "find string not present".into(),
            base_len: base.len(),
            find: find.to_string(),
            replace: replace.to_string(),
        }),
        1 => Ok(base.replacen(find, replace, 1)),
        n => Err(OtConflict {
            reason: format!("{n} occurrences — refuse ambiguous OT patch"),
            base_len: base.len(),
            find: find.to_string(),
            replace: replace.to_string(),
        }),
    }
}

#[derive(Debug, Clone)]
pub struct OtConflict {
    pub reason: String,
    pub base_len: usize,
    pub find: String,
    pub replace: String,
}

pub fn merge_event(kind: &str, detail: Value) {
    crate::events::publish(format!("crdt.{kind}"), detail);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn higher_counter_wins_across_actors() {
        let a = Clock {
            actor: "a".into(),
            counter: 1,
        };
        let b = Clock {
            actor: "b".into(),
            counter: 2,
        };
        assert_eq!(pick_lww(&a, &b), MergePick::TakeIncoming);
        assert_eq!(pick_lww(&b, &a), MergePick::KeepLocal);
    }

    #[test]
    fn same_counter_different_actors_are_concurrent_for_observations() {
        let a = Clock {
            actor: "alice".into(),
            counter: 5,
        };
        let b = Clock {
            actor: "bob".into(),
            counter: 5,
        };
        assert_eq!(pick_observation_content(&a, &b), MergePick::Concurrent);
    }

    #[test]
    fn ot_refuses_ambiguous_find() {
        let err = ot_line_patch("aa aa", "aa", "b").unwrap_err();
        assert!(err.reason.contains("2 occurrences"));
    }

    #[test]
    fn ot_applies_unique_find() {
        assert_eq!(
            ot_line_patch("hello world", "world", "cuba").unwrap(),
            "hello cuba"
        );
    }
}
