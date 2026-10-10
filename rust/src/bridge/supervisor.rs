//! The watcher of `bridge run`: it looks for the host as soon as the bridge starts, checks it
//! every 60 s while connected, retries after 5, 10, 20, 40 and 60 s while it is not, and tells
//! the person at the desk only when something changed.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use super::Bridge;
use super::notify::Notifier;

pub struct Supervisor {
    bridge: Arc<Bridge>,
    notifier: Arc<dyn Notifier>,
    exe: PathBuf,
}

impl Supervisor {
    /// `exe` is the binary the notice's buttons run (`bridge reconnect`).
    pub fn new(bridge: Arc<Bridge>, notifier: Arc<dyn Notifier>, exe: PathBuf) -> Self {
        Supervisor {
            bridge,
            notifier,
            exe,
        }
    }

    /// One check of the host, the notices it changes, and how long to wait before the next.
    /// The loop of `bridge run` sleeps that long; a test moves its clock by it instead.
    pub async fn tick(&mut self) -> Duration {
        unimplemented!("pasada verde: B1")
    }
}

#[cfg(test)]
mod tests {
    use super::super::fake_host::{
        CredStatus, FakeHost, FakeNet, HOST_NAME, Harness, Identity, Prepared, RecordingNotifier,
    };
    use super::super::notify::Notice;
    use super::*;
    use crate::accounts;

    const EXE: &str = r"C:\Users\ana\AppData\Local\MemoryIndustry\bin\memory-industry.exe";

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    /// A paired bridge with its watcher, a host at the address it knows, and a clock that only
    /// moves when the watcher says how long it would sleep.
    struct Watch {
        id: Identity,
        host: FakeHost,
        p: Prepared,
        notes: Arc<RecordingNotifier>,
        sup: Supervisor,
    }

    async fn watch_on(id: Identity, host: FakeHost) -> Watch {
        let state = id.state(host.addr().port(), vec![host.addr()], Some(host.addr()));
        let p = Harness::prepare(&state, FakeNet::new());
        let notes = Arc::new(RecordingNotifier::new(p.clock.clone()));
        let sup = Supervisor::new(p.bridge.clone(), notes.clone(), PathBuf::from(EXE));
        Watch {
            id,
            host,
            p,
            notes,
            sup,
        }
    }

    async fn watch(host_up: bool) -> Watch {
        let id = Identity::new();
        let host = FakeHost::start(id.host_id, vec![id.fake_cred()]).await;
        host.set_down(!host_up);
        watch_on(id, host).await
    }

    impl Watch {
        /// One check, then the clock moves by the wait the watcher asked for.
        async fn step(&mut self) -> Duration {
            let wait = self.sup.tick().await;
            assert!(
                wait > Duration::ZERO,
                "a wait of zero is a busy loop against the host"
            );
            assert!(
                wait <= secs(60),
                "no wait is longer than the 60 s cap of B1, got {wait:?}"
            );
            self.p.clock.advance(wait);
            wait
        }

        async fn run_until(&mut self, at: Duration) {
            for _ in 0..1000 {
                if self.p.clock.elapsed() >= at {
                    return;
                }
                self.step().await;
            }
            panic!("the watcher did not reach {at:?} in 1000 checks");
        }

        fn now(&self) -> Duration {
            self.p.clock.elapsed()
        }

        fn notices(&self) -> Vec<(Duration, Notice)> {
            self.notes.notices()
        }

        fn count(&self, kind: fn(&Notice) -> bool) -> usize {
            self.notices().iter().filter(|(_, n)| kind(n)).count()
        }
    }

    fn is_connected(n: &Notice) -> bool {
        matches!(n, Notice::Connected { .. })
    }

    fn is_lost(n: &Notice) -> bool {
        matches!(n, Notice::Lost { .. })
    }

    fn is_reconnected(n: &Notice) -> bool {
        matches!(n, Notice::Reconnected { .. })
    }

    fn is_needs_pair(n: &Notice) -> bool {
        matches!(n, Notice::NeedsPair { .. })
    }

    /// Escenario: al arrancar se conecta y avisa una vez.
    #[tokio::test]
    async fn al_arrancar_se_conecta_y_avisa_connected_una_sola_vez() {
        let mut w = watch(true).await;

        w.run_until(secs(600)).await;

        let notices = w.notices();
        assert_eq!(
            notices.len(),
            1,
            "ten minutes connected, one notice: a check every 60 s that announced itself would \
             be a toast a minute. Got {notices:?}"
        );
        match &notices[0] {
            (at, Notice::Connected { host_name, address }) => {
                assert_eq!(
                    *at,
                    Duration::ZERO,
                    "the bridge looks for the host as it starts, not after a first wait"
                );
                assert_eq!(host_name, HOST_NAME, "the notice names the host");
                assert_eq!(*address, w.host.addr(), "and the address it answered on");
            }
            other => panic!("the first notice is Connected, got {other:?}"),
        }
        assert!(
            w.host.accepted() >= 10,
            "anchor: the watcher kept checking the host while it said nothing (one check every \
             60 s for 10 min), so the silence is not a watcher that stopped. Connections: {}",
            w.host.accepted()
        );
    }

    /// Escenario: la red tarda en levantar al iniciar sesión.
    #[tokio::test]
    async fn si_la_madre_responde_a_los_70_s_no_hay_lost_y_hay_un_connected() {
        let mut w = watch(false).await;

        w.run_until(secs(70)).await;
        assert!(
            w.host.accepted() > 0,
            "anchor: the watcher asked the host while it was down"
        );
        assert!(
            w.notices().is_empty(),
            "70 s without a host is a network that is still starting, not a host that was lost. \
             Got {:?}",
            w.notices()
        );
        w.host.set_down(false);
        w.run_until(secs(300)).await;

        assert_eq!(
            w.count(is_lost),
            0,
            "the host answered before 90 s without success: no Lost. Got {:?}",
            w.notices()
        );
        assert_eq!(
            w.count(is_reconnected),
            0,
            "nothing was lost to come back from"
        );
        let connected: Vec<Duration> = w
            .notices()
            .into_iter()
            .filter(|(_, n)| is_connected(n))
            .map(|(at, _)| at)
            .collect();
        assert_eq!(
            connected.len(),
            1,
            "one Connected, the first success. Got {:?}",
            w.notices()
        );
        assert!(
            connected[0] >= secs(70) && connected[0] <= secs(130),
            "the Connected comes at the first retry after the host answered, and the retries never \
             wait more than 60 s. Came at {:?}",
            connected[0]
        );
    }

    /// Escenario: la madre se pierde y vuelve.
    #[tokio::test]
    async fn si_la_madre_se_pierde_90_s_hay_un_solo_lost_y_al_volver_un_reconnected() {
        let mut w = watch(true).await;
        w.step().await;
        assert_eq!(w.count(is_connected), 1, "anchor: connected at the start");
        let last_good_at = Duration::ZERO;
        w.host.set_down(true);

        w.run_until(secs(600)).await;

        let lost: Vec<(Duration, Notice)> = w
            .notices()
            .into_iter()
            .filter(|(_, n)| is_lost(n))
            .collect();
        assert_eq!(
            lost.len(),
            1,
            "nine minutes without the host, many failed checks, one Lost: a notice per failure \
             is a window a minute on a laptop away from the office. Got {:?}",
            w.notices()
        );
        let (at, notice) = &lost[0];
        assert!(
            *at - last_good_at >= secs(90),
            "Lost came {at:?} after the last success: before 90 s it is a network still starting"
        );
        assert!(
            *at - last_good_at <= secs(150),
            "Lost came {at:?} after the last success: the 90 s passed and the next check, at most \
             60 s later, should have said it"
        );
        let Notice::Lost {
            host_name,
            last_good,
            fingerprint,
            down_for_secs,
            report,
            exe,
        } = notice
        else {
            unreachable!("filtered on Lost")
        };
        assert_eq!(host_name, HOST_NAME, "the window names the host");
        assert_eq!(
            *last_good,
            Some(w.host.addr()),
            "and the last address that worked"
        );
        assert_eq!(
            *fingerprint,
            accounts::fingerprint(&w.id.host_id),
            "and the short fingerprint the operator compares with the host's screen"
        );
        assert!(
            *down_for_secs >= 90,
            "and how long it has been without the host: {down_for_secs} s"
        );
        assert!(
            report.contains(&w.host.addr().to_string()),
            "the report says what was tried, the last good address among it. Got: {report}"
        );
        assert_eq!(
            exe,
            &PathBuf::from(EXE),
            "and the binary the buttons of the window run"
        );
        let shown = serde_json::to_string(notice).expect("a notice is JSON");
        for secret in [
            w.id.secret_hex(),
            w.id.credential_id.simple().to_string(),
            w.id.credential_id.to_string(),
        ] {
            assert!(
                !shown
                    .to_ascii_lowercase()
                    .contains(&secret.to_ascii_lowercase()),
                "a notice goes to a window and to the clipboard: no secret, no credential id"
            );
        }

        w.host.set_down(false);
        let back_at = w.now();
        w.run_until(back_at + secs(120)).await;

        let reconnected: Vec<(Duration, Notice)> = w
            .notices()
            .into_iter()
            .filter(|(_, n)| is_reconnected(n))
            .collect();
        assert_eq!(
            reconnected.len(),
            1,
            "the host is back: one Reconnected. Got {:?}",
            w.notices()
        );
        assert!(
            reconnected[0].0 <= back_at + secs(60),
            "no retry waits more than 60 s, so the Reconnected comes within a minute of the host"
        );
        assert!(
            matches!(&reconnected[0].1, Notice::Reconnected { host_name, address }
                if host_name == HOST_NAME && *address == w.host.addr()),
            "it names the host and the address: {:?}",
            reconnected[0].1
        );
        assert_eq!(
            w.count(is_connected),
            1,
            "coming back is Reconnected, not a second Connected"
        );
        assert_eq!(w.count(is_lost), 1, "and no Lost while connected");

        w.host.set_down(true);
        let gone_again = w.now();
        w.run_until(gone_again + secs(300)).await;

        assert_eq!(
            w.count(is_lost),
            2,
            "one Lost per loss, not one per run: after a recovery, a new loss is news again"
        );
    }

    /// B1: connected, a light check every 60 s; without the host, 5, 10, 20, 40 and then 60 s.
    /// Each retry asks the host for real: a retry answered from the remembered failure would
    /// leave a host that came back at 6 s unseen for a minute.
    #[tokio::test]
    async fn sin_madre_reintenta_a_5_10_20_40_y_60_s_y_cada_reintento_llega_a_la_madre() {
        let mut w = watch(false).await;

        let mut waits = Vec::new();
        let mut reached = Vec::new();
        for _ in 0..7 {
            waits.push(w.step().await);
            reached.push(w.host.accepted());
        }

        assert_eq!(
            waits,
            [5, 10, 20, 40, 60, 60, 60].map(secs),
            "the backoff of B1, capped at 60 s"
        );
        assert!(
            reached.windows(2).all(|pair| pair[1] > pair[0]) && reached[0] > 0,
            "every retry reached the host's address; connections seen after each check: {reached:?}"
        );

        w.host.set_down(false);
        assert_eq!(
            w.step().await,
            secs(60),
            "found: the next check is the light one, a minute later"
        );
        assert_eq!(
            w.step().await,
            secs(60),
            "and stays a minute while connected"
        );

        w.host.set_down(true);
        let after_loss = [w.step().await, w.step().await, w.step().await];
        assert_eq!(
            after_loss,
            [5, 10, 20].map(secs),
            "the check that finds the host gone waits like any failure: the backoff starts again \
             at 5 s, not at the 60 s where the last loss left it"
        );
    }

    /// B1: a revoked credential is not a lost host. Retrying does not fix it, and Lost would
    /// tell the person to look at the network.
    #[tokio::test]
    async fn una_credencial_revocada_avisa_needs_pair_una_sola_vez_y_ningun_lost() {
        let mut w = watch(true).await;
        w.host
            .set_cred_status(w.id.credential_id, CredStatus::Revoked);

        w.run_until(secs(600)).await;

        let notices = w.notices();
        assert_eq!(
            notices.len(),
            1,
            "ten minutes revoked, one notice. Got {notices:?}"
        );
        match &notices[0].1 {
            Notice::NeedsPair {
                host_name,
                fingerprint,
                reason,
                exe,
            } => {
                assert_eq!(host_name, HOST_NAME);
                assert_eq!(*fingerprint, accounts::fingerprint(&w.id.host_id));
                assert!(
                    reason.contains("bridge pair"),
                    "the reason says the one command that fixes it. Got: {reason}"
                );
                assert_eq!(exe, &PathBuf::from(EXE));
            }
            other => panic!("a revoked credential is NeedsPair, got {other:?}"),
        }
        assert_eq!(w.count(is_lost), 0);
        assert_eq!(w.count(is_needs_pair), 1);
    }

    /// B1: a host that answers with another fingerprint is another installation. The pinned
    /// one is not adopted on the bridge's own authority; the person is asked to pair again.
    #[tokio::test]
    async fn una_madre_con_otra_huella_avisa_needs_pair_una_sola_vez() {
        let id = Identity::new();
        let other = uuid::Uuid::new_v4();
        let replaced = FakeHost::start(other, vec![id.fake_cred()]).await;
        let mut w = watch_on(id, replaced).await;

        w.run_until(secs(600)).await;

        let notices = w.notices();
        assert_eq!(
            notices.len(),
            1,
            "ten minutes in front of another installation, one notice. Got {notices:?}"
        );
        match &notices[0].1 {
            Notice::NeedsPair { reason, .. } => {
                assert!(
                    reason.contains(&accounts::fingerprint(&other)),
                    "the reason shows the fingerprint that was seen. Got: {reason}"
                );
            }
            other => panic!("another fingerprint is NeedsPair, got {other:?}"),
        }
        assert_eq!(
            w.count(is_connected),
            0,
            "the other installation is never 'connected'"
        );
    }
}
