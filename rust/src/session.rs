use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, RwLock};

use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActiveSession {
    pub session_id: Uuid,
    pub project_id: Option<Uuid>,
}

static ACTIVE: RwLock<Option<ActiveSession>> = RwLock::new(None);

static PER_CLIENT: LazyLock<RwLock<HashMap<String, ActiveSession>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

static DAEMON: AtomicBool = AtomicBool::new(false);

tokio::task_local! {
    static CLIENT: String;
    static CLIENT_LABEL: String;
    static MCP_SESSION: String;
    static SCOPE: Scope;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Full,
    Peer,
}

pub const PEER_VERBS: [(&str, &str); 3] = [
    ("cuba_sync", "status"),
    ("cuba_sync", "pull"),
    ("cuba_sync", "notify"),
];

pub fn current_scope() -> Scope {
    SCOPE.try_with(|s| *s).unwrap_or(Scope::Full)
}

pub async fn with_scope<F, R>(scope: Scope, fut: F) -> R
where
    F: std::future::Future<Output = R>,
{
    SCOPE.scope(scope, fut).await
}

#[cfg(test)]
pub static GLOBAL_STATE_GUARD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub fn enable_daemon_mode() {
    DAEMON.store(true, Ordering::Relaxed);
}

pub fn daemon_mode() -> bool {
    DAEMON.load(Ordering::Relaxed)
}

pub fn current_client() -> Option<String> {
    CLIENT.try_with(|c| c.clone()).ok()
}

pub fn configured_client_label() -> Option<String> {
    client_label_from(
        std::env::var("MEMORY_INDUSTRY_CLIENT_ID").ok().as_deref(),
        std::env::var("CUBA_CLIENT_ID").ok().as_deref(),
    )
}

/// Split from the lookup so it can be checked without touching the process
/// environment. `set_var` is unsound in a multi-threaded program, and a test
/// that mutates the environment can make an unrelated one read a torn value —
/// which is how this very assertion failed once in a hundred gate runs.
fn client_label_from(preferred: Option<&str>, legacy: Option<&str>) -> Option<String> {
    preferred
        .or(legacy)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn current_client_label() -> Option<String> {
    CLIENT_LABEL
        .try_with(|c| c.clone())
        .ok()
        .or_else(|| current_client().map(client_label_of))
        .or_else(configured_client_label)
}

pub fn current_mcp_session() -> Option<String> {
    MCP_SESSION
        .try_with(|s| s.clone())
        .ok()
        .filter(|s| !s.is_empty())
}

/// Where a request came from, for the single purpose of telling two machines
/// apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// The caller is on this machine. Every loopback client shares one bucket,
    /// which is exactly what every install does today.
    Local,
    /// Another machine, named by its `Mcp-Machine-Id` or, failing that, by the
    /// address it connected from.
    Remote(String),
}

pub fn bind_key(client: &str, mcp_session: Option<&str>, origin: &Origin) -> String {
    // The machine goes before the `::` so that forget_client, which purges by
    // the `{key}::` prefix, still reaches every session belonging to one
    // machine. Local carries no suffix at all: that keeps the key byte for
    // byte what every install running today already has.
    let who = match origin {
        Origin::Local => client.to_string(),
        Origin::Remote(machine) => format!("{client}@{machine}"),
    };
    match mcp_session {
        Some(s) if !s.is_empty() => format!("{who}::{s}"),
        _ => who,
    }
}

fn client_label_of(bind: String) -> String {
    bind.split("::")
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(&bind)
        .to_string()
}

pub async fn with_client<F, R>(key: String, fut: F) -> R
where
    F: std::future::Future<Output = R>,
{
    CLIENT.scope(key, fut).await
}

pub async fn with_identity<F, R>(label: String, mcp_session: Option<String>, fut: F) -> R
where
    F: std::future::Future<Output = R>,
{
    let bind = bind_key(&label, mcp_session.as_deref(), &Origin::Local);
    let session = mcp_session.unwrap_or_default();
    CLIENT_LABEL
        .scope(label, MCP_SESSION.scope(session, CLIENT.scope(bind, fut)))
        .await
}

pub fn forget_client(key: &str) {
    let prefix = format!("{key}::");
    if let Ok(mut guard) = PER_CLIENT.write() {
        guard.remove(key);
        guard.retain(|k, _| !k.starts_with(&prefix));
    }
    if let Ok(mut guard) = CLIENT_ROOTS.write() {
        guard.remove(key);
        guard.retain(|k, _| !k.starts_with(&prefix));
    }
}

static CLIENT_ROOTS: LazyLock<RwLock<HashMap<String, Uuid>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

pub fn remember_client_root(key: &str, project_id: Uuid) {
    if let Ok(mut guard) = CLIENT_ROOTS.write() {
        guard.insert(key.to_string(), project_id);
    }
}

tokio::task_local! {
    static ROOT_PROJECT: Uuid;
}

pub async fn with_root_project<F, R>(project: Option<Uuid>, fut: F) -> R
where
    F: std::future::Future<Output = R>,
{
    match project {
        Some(id) => ROOT_PROJECT.scope(id, fut).await,
        None => fut.await,
    }
}

pub fn client_root_project() -> Option<Uuid> {
    ROOT_PROJECT.try_with(|id| *id).ok()
}

pub fn client_root_project_for(key: &str) -> Option<Uuid> {
    CLIENT_ROOTS.read().ok()?.get(key).copied()
}

pub fn root_to_project_name(uri: &str) -> Option<String> {
    let path = uri.strip_prefix("file://").unwrap_or(uri);
    let decoded: String = {
        let bytes = path.as_bytes();
        let mut out = String::with_capacity(path.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%'
                && i + 2 < bytes.len()
                && let Ok(byte) = u8::from_str_radix(&path[i + 1..i + 3], 16)
            {
                out.push(byte as char);
                i += 3;
                continue;
            }
            out.push(bytes[i] as char);
            i += 1;
        }
        out
    };
    let name = decoded.trim_end_matches('/').rsplit('/').next()?;
    if name.is_empty() || name == "." {
        return None;
    }
    Some(name.to_string())
}

pub fn all_active() -> Vec<ActiveSession> {
    PER_CLIENT
        .read()
        .map(|g| g.values().copied().collect())
        .unwrap_or_default()
}

pub fn set(session_id: Uuid, project_id: Option<Uuid>) {
    let value = ActiveSession {
        session_id,
        project_id,
    };
    match current_client() {
        Some(key) => {
            if let Ok(mut guard) = PER_CLIENT.write() {
                guard.insert(key, value);
            }
        }
        None => {
            if let Ok(mut guard) = ACTIVE.write() {
                *guard = Some(value);
            }
        }
    }
}

pub fn clear() {
    match current_client() {
        Some(key) => {
            if let Ok(mut guard) = PER_CLIENT.write() {
                guard.remove(&key);
            }
        }
        None => {
            if let Ok(mut guard) = ACTIVE.write() {
                *guard = None;
            }
        }
    }
}

pub fn get() -> Option<ActiveSession> {
    if let Some(key) = current_client() {
        return PER_CLIENT.read().ok().and_then(|g| g.get(&key).copied());
    }
    if daemon_mode() {
        return None;
    }
    ACTIVE.read().ok().and_then(|g| *g)
}

pub fn project_id() -> Option<Uuid> {
    get().and_then(|s| s.project_id)
}

pub fn session_id() -> Option<Uuid> {
    get().map(|s| s.session_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_client_id_is_not_a_workspace_identity() {
        assert_eq!(client_label_from(Some("   "), None), None);
        assert_eq!(client_label_from(Some(""), None), None);
        assert_eq!(client_label_from(None, None), None);
        assert_eq!(
            client_label_from(Some(" cursor "), None),
            Some("cursor".to_string()),
            "the label is trimmed, because a config file with a trailing space would otherwise key a whole second workspace"
        );
        assert_eq!(
            client_label_from(None, Some("legacy")),
            Some("legacy".to_string()),
            "CUBA_CLIENT_ID still answers for one release"
        );
        assert_eq!(
            client_label_from(Some("new"), Some("legacy")),
            Some("new".to_string()),
            "the preferred spelling wins when both are set"
        );
    }

    #[test]
    fn a_protocol_session_is_not_the_same_client_as_the_window() {
        assert_eq!(bind_key("cursor", None, &Origin::Local), "cursor");
        assert_eq!(
            bind_key("cursor", Some("chat-9"), &Origin::Local),
            "cursor::chat-9",
            "two Cursor chats share Mcp-Client-Id=cursor; without the protocol session \
             they inherit each other's jornada"
        );
        assert_eq!(bind_key("cursor", Some(""), &Origin::Local), "cursor");
    }

    #[test]
    fn a_root_uri_becomes_the_name_of_the_directory_it_points_at() {
        assert_eq!(
            root_to_project_name("file:///home/leandro/proyectos/MCP/cuba-memorys").as_deref(),
            Some("cuba-memorys")
        );
        assert_eq!(
            root_to_project_name("file:///home/leandro/proyectos/MCP/cuba-memorys/").as_deref(),
            Some("cuba-memorys"),
            "a trailing slash is not a directory called empty string"
        );
        assert_eq!(
            root_to_project_name("file:///home/leandro/proyectos/pedido/%5Bid%5D").as_deref(),
            Some("[id]"),
            "Claude Code percent-encodes its roots — measured 16-ago-2026, it announced \
             `.../pedido/%5Bid%5D` for a Next.js dynamic route. Left encoded, that name reaches \
             brain_projects as literal %5Bid%5D and never matches the same directory again"
        );
        assert_eq!(root_to_project_name("file:///").as_deref(), None);
    }

    #[tokio::test]
    async fn the_root_is_remembered_per_client_not_globally() {
        let one = Uuid::new_v4();
        let two = Uuid::new_v4();
        remember_client_root("claude-code@a", one);
        remember_client_root("claude-code@b", two);

        assert_eq!(client_root_project_for("claude-code@a"), Some(one));
        assert_eq!(
            client_root_project_for("claude-code@b"),
            Some(two),
            "every Claude Code instance sends the same clientInfo.name, so if the root were \
             kept in one global slot the last client to connect would silently retarget every \
             other client's writes at its own project"
        );
        assert_eq!(client_root_project_for("never-seen"), None);

        forget_client("claude-code@a");
        assert_eq!(
            client_root_project_for("claude-code@a"),
            None,
            "a client that goes away must not leave its project behind for whoever reuses the key"
        );
        forget_client("claude-code@b");
    }

    #[tokio::test]
    async fn set_get_clear_roundtrip() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let sid = Uuid::new_v4();
        let pid = Uuid::new_v4();
        clear();
        assert_eq!(
            get(),
            None,
            "otro test escribió el ACTIVE global entre el clear y esta lectura: sin el cerrojo \
             de arriba esta suite falla ~5% de las veces y nadie se cree su verde"
        );

        set(sid, Some(pid));
        assert_eq!(session_id(), Some(sid));
        assert_eq!(project_id(), Some(pid));

        set(sid, None);
        assert_eq!(project_id(), None);

        clear();
        assert_eq!(get(), None);
        assert_eq!(project_id(), None);
    }

    #[tokio::test]
    async fn clients_do_not_see_each_others_sessions() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();

        with_client("editor-a".to_string(), async {
            set(a, None);
            assert_eq!(session_id(), Some(a));
        })
        .await;

        with_client("editor-b".to_string(), async {
            assert_eq!(session_id(), None, "b must not inherit a's session");
            set(b, None);
            assert_eq!(session_id(), Some(b));
        })
        .await;

        with_client("editor-a".to_string(), async {
            assert_eq!(session_id(), Some(a), "a's session survived b's write");
        })
        .await;

        forget_client("editor-a");
        forget_client("editor-b");
    }

    #[tokio::test]
    async fn daemon_hides_the_global_session_from_unscoped_tasks() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let sid = Uuid::new_v4();
        set(sid, None);
        assert_eq!(
            session_id(),
            Some(sid),
            "global still readable in stdio mode"
        );

        enable_daemon_mode();
        assert_eq!(
            session_id(),
            None,
            "a background task must not adopt a stray global session"
        );

        DAEMON.store(false, Ordering::Relaxed);
        clear();
    }

    #[tokio::test]
    async fn forget_client_drops_the_row() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let sid = Uuid::new_v4();
        with_client("ephemeral".to_string(), async {
            set(sid, None);
            assert_eq!(session_id(), Some(sid));
        })
        .await;

        forget_client("ephemeral");

        with_client("ephemeral".to_string(), async {
            assert_eq!(session_id(), None, "row is gone after forget_client");
        })
        .await;
    }

    #[tokio::test]
    async fn with_identity_exposes_the_protocol_session() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        with_identity("cursor".into(), Some("chat-a".into()), async {
            assert_eq!(
                current_mcp_session().as_deref(),
                Some("chat-a"),
                "whoami.mcp_session and the bind key both read this"
            );
            assert_eq!(current_client_label().as_deref(), Some("cursor"));
            assert_eq!(
                current_client().as_deref(),
                Some("cursor::chat-a"),
                "bind_key must join label and protocol session"
            );
        })
        .await;
        assert_eq!(
            current_mcp_session(),
            None,
            "the protocol session must not leak outside with_identity"
        );
    }

    #[tokio::test]
    async fn an_empty_protocol_session_is_not_a_session() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        with_identity("cursor".into(), Some(String::new()), async {
            assert_eq!(
                current_mcp_session(),
                None,
                "empty Mcp-Session-Id must not invent a session (filter !is_empty)"
            );
            assert_eq!(
                current_client().as_deref(),
                Some("cursor"),
                "bind_key falls back to the bare client when the session is blank"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn client_label_strips_the_protocol_session_suffix() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        with_client("cursor::chat-a".into(), async {
            assert_eq!(
                current_client_label().as_deref(),
                Some("cursor"),
                "CLIENT holds the bind key; the label is the part before ::"
            );
        })
        .await;
    }

    #[test]
    fn a_configured_client_id_is_an_identity() {
        let prev_a = std::env::var("MEMORY_INDUSTRY_CLIENT_ID").ok();
        let prev_b = std::env::var("CUBA_CLIENT_ID").ok();
        unsafe {
            std::env::set_var("MEMORY_INDUSTRY_CLIENT_ID", "desk-one");
            std::env::remove_var("CUBA_CLIENT_ID");
        }
        assert_eq!(configured_client_label().as_deref(), Some("desk-one"));
        unsafe {
            match prev_a {
                Some(v) => std::env::set_var("MEMORY_INDUSTRY_CLIENT_ID", v),
                None => std::env::remove_var("MEMORY_INDUSTRY_CLIENT_ID"),
            }
            match prev_b {
                Some(v) => std::env::set_var("CUBA_CLIENT_ID", v),
                None => std::env::remove_var("CUBA_CLIENT_ID"),
            }
        }
    }

    #[tokio::test]
    async fn forget_client_also_drops_protocol_session_keys() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let one = Uuid::new_v4();
        let sid = Uuid::new_v4();
        remember_client_root("cursor::chat-a", one);
        with_client("cursor::chat-a".into(), async {
            set(sid, None);
            assert_eq!(session_id(), Some(sid));
        })
        .await;
        assert_eq!(client_root_project_for("cursor::chat-a"), Some(one));

        forget_client("cursor");

        assert_eq!(
            client_root_project_for("cursor::chat-a"),
            None,
            "forget_client must retain(!starts_with(prefix)) on CLIENT_ROOTS — without \
             the ! it would keep every protocol-session key and chat-a would leak"
        );
        with_client("cursor::chat-a".into(), async {
            assert_eq!(
                session_id(),
                None,
                "forget_client must retain(!starts_with(prefix)) on PER_CLIENT too — \
                 the line-125 mutant deleted only that ! and the roots-only assert \
                 could not see it"
            );
        })
        .await;
    }

    #[test]
    fn two_machines_that_share_a_client_id_do_not_share_a_session() {
        let one = bind_key("claude-code", None, &Origin::Remote("10.0.0.2".into()));
        let two = bind_key("claude-code", None, &Origin::Remote("10.0.0.3".into()));
        assert_ne!(
            one, two,
            "the key saw only the client id, so two workstations configured from the same example config landed in one bucket and inherited each other's open jornada and root project. Nothing errored: the corruption is silent, and on a LAN daemon it is not a risk but the default."
        );
    }

    #[test]
    fn a_local_caller_keys_exactly_as_it_did_before() {
        assert_eq!(
            bind_key("cursor", None, &Origin::Local),
            "cursor",
            "every install running today is loopback. If the key changed shape for them they would all lose their open session on upgrade, and v029 would break."
        );
        assert_eq!(
            bind_key("cursor", Some("chat-9"), &Origin::Local),
            "cursor::chat-9"
        );
    }

    #[test]
    fn a_remote_key_still_carries_its_protocol_session_and_its_label() {
        let key = bind_key(
            "claude-code",
            Some("chat-a"),
            &Origin::Remote("ws-7".into()),
        );
        assert_eq!(
            key, "claude-code@ws-7::chat-a",
            "the machine goes before the :: so that forget_client, which purges by the {{key}}:: prefix, still reaches every session of one machine"
        );
        assert_eq!(
            client_label_of(key.clone()),
            "claude-code@ws-7",
            "two machines are two workspaces; the label has to say which one"
        );
    }
}
