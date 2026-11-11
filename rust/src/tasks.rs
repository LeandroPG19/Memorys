use std::future::Future;
use std::sync::LazyLock;
use std::time::Duration;

use tokio_util::task::TaskTracker;

static TRACKER: LazyLock<TaskTracker> = LazyLock::new(TaskTracker::new);

/// What the shutdown logs when `drain` gives up. The prefix is the one `e2e_all_tools.py` reads.
pub const LOST_AT_SHUTDOWN: &str = "background tasks did not finish before shutdown — an episode may be left without its vector until `memory-industry reembed` (cuba_zafra action=reembed) or the next REM cycle fills it, and a near-duplicate consolidation runs again on the next add to that entity";

pub fn spawn<F>(future: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    TRACKER.spawn(future);
}

pub fn inflight() -> usize {
    TRACKER.len()
}

pub async fn drain(timeout: Duration) -> usize {
    TRACKER.close();
    if TRACKER.is_empty() {
        return 0;
    }
    tracing::debug!(inflight = TRACKER.len(), "draining background tasks");
    match tokio::time::timeout(timeout, TRACKER.wait()).await {
        Ok(()) => 0,
        Err(_) => TRACKER.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn drain_waits_for_inflight_work() {
        let counter = Arc::new(AtomicUsize::new(0));
        for _ in 0..8 {
            let c = counter.clone();
            spawn(async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                c.fetch_add(1, Ordering::SeqCst);
            });
        }
        let lost = drain(Duration::from_secs(5)).await;
        assert_eq!(lost, 0, "no task should be lost");
        assert_eq!(
            counter.load(Ordering::SeqCst),
            8,
            "every task must run to completion before drain returns"
        );
    }

    #[test]
    fn the_shutdown_message_names_what_is_lost_and_what_restores_it() {
        let prefix_that_e2e_all_tools_reads = "background tasks did not finish before shutdown";
        assert!(
            LOST_AT_SHUTDOWN.starts_with(prefix_that_e2e_all_tools_reads),
            "e2e_all_tools.py (BACKGROUND_LOST) looks for this prefix in the log of a process that \
             exits with work in flight: {LOST_AT_SHUTDOWN}"
        );
        for what_it_has_to_say in [
            "episode",
            "memory-industry reembed",
            "cuba_zafra action=reembed",
            "REM",
        ] {
            assert!(
                LOST_AT_SHUTDOWN.contains(what_it_has_to_say),
                "what is lost is an episode's vector, and the way back is one of `reembed` (either \
                 entry) or the next REM cycle; the message never says {what_it_has_to_say:?}: \
                 {LOST_AT_SHUTDOWN}"
            );
        }
        assert!(
            !LOST_AT_SHUTDOWN.contains("some embeddings were not persisted"),
            "the old text blamed observations too, and promised a recovery that did not exist for \
             episodes: {LOST_AT_SHUTDOWN}"
        );
    }
}
