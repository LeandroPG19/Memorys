//! In-process event bus for SSE live views (artifacts, peers, CRDT merges).
//! Short-lived tickets let EventSource auth without putting the long-lived bearer in query logs.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::broadcast;
use uuid::Uuid;

const CAPACITY: usize = 256;
const TICKET_TTL: Duration = Duration::from_secs(120);

static BUS: OnceLock<broadcast::Sender<BusEvent>> = OnceLock::new();
static TICKETS: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();

#[derive(Clone, Debug, serde::Serialize)]
pub struct BusEvent {
    pub kind: String,
    pub at: String,
    pub payload: Value,
}

fn sender() -> &'static broadcast::Sender<BusEvent> {
    BUS.get_or_init(|| {
        let (tx, _rx) = broadcast::channel(CAPACITY);
        tx
    })
}

fn tickets() -> &'static Mutex<HashMap<String, Instant>> {
    TICKETS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn subscribe() -> broadcast::Receiver<BusEvent> {
    sender().subscribe()
}

pub fn publish(kind: impl Into<String>, payload: Value) {
    let event = BusEvent {
        kind: kind.into(),
        at: chrono::Utc::now().to_rfc3339(),
        payload,
    };
    let _ = sender().send(event);
}

/// Issue a short-lived SSE ticket after a normal bearer check. Not a substitute for admin auth.
pub fn issue_ticket() -> String {
    let ticket = Uuid::new_v4().to_string();
    let mut guard = tickets().lock().expect("sse tickets mutex");
    let now = Instant::now();
    guard.retain(|_, exp| *exp > now);
    guard.insert(ticket.clone(), now + TICKET_TTL);
    ticket
}

pub fn ticket_valid(ticket: &str) -> bool {
    let t = ticket.trim();
    if t.is_empty() {
        return false;
    }
    let mut guard = tickets().lock().expect("sse tickets mutex");
    let now = Instant::now();
    guard.retain(|_, exp| *exp > now);
    guard.get(t).is_some_and(|exp| *exp > now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_subscriber_hears_what_was_published() {
        let mut rx = subscribe();
        publish("test.ping", serde_json::json!({"n": 1}));
        let got = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out")
            .expect("channel closed");
        assert_eq!(got.kind, "test.ping");
        assert_eq!(got.payload["n"], 1);
    }

    #[test]
    fn tickets_expire_and_validate() {
        let t = issue_ticket();
        assert!(ticket_valid(&t));
        assert!(!ticket_valid("nope"));
    }
}
