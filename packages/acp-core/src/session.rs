// ACP Core module implements session behavior.
// 翻译自 packages/acp-core/src/session.ts

use std::collections::HashMap;
use std::sync::Mutex;

use uuid::Uuid;

use crate::numeric_options::resolve_integer_option;
use crate::types::{AbortController, AcpSession};

pub struct CreateSessionParams {
    pub session_key: String,
    pub cwd: String,
    pub session_id: Option<String>,
    pub ledger_session_id: Option<String>,
}

pub trait AcpSessionStore: Send + Sync {
    /// Creates or refreshes an in-memory ACP session under the supplied session id.
    fn create_session(&self, params: CreateSessionParams) -> AcpSession;
    fn has_session(&self, session_id: &str) -> bool;
    fn get_session(&self, session_id: &str) -> Option<AcpSession>;
    fn get_session_by_run_id(&self, run_id: &str) -> Option<AcpSession>;
    /// Binds an active runtime run to a session so cancel/close can abort it later.
    fn set_active_run(&self, session_id: &str, run_id: &str, abort_controller: AbortController);
    fn clear_active_run(&self, session_id: &str);
    fn cancel_active_run(&self, session_id: &str) -> bool;
    fn delete_session(&self, session_id: &str) -> bool;
    fn clear_all_sessions_for_test(&self);
}

pub struct AcpSessionStoreOptions {
    pub max_sessions: Option<i64>,
    pub idle_ttl_ms: Option<i64>,
    pub now: Option<fn() -> i64>,
}

impl Default for AcpSessionStoreOptions {
    fn default() -> Self {
        Self {
            max_sessions: None,
            idle_ttl_ms: None,
            now: None,
        }
    }
}

const DEFAULT_MAX_SESSIONS: i64 = 5_000;
const DEFAULT_IDLE_TTL_MS: i64 = 24 * 60 * 60 * 1_000;

/// Creates the bounded in-memory ACP session registry used by local ACP runtime clients.
pub fn create_in_memory_session_store(options: AcpSessionStoreOptions) -> impl AcpSessionStore {
    InMemorySessionStore::new(options)
}

pub struct InMemorySessionStore {
    max_sessions: i64,
    idle_ttl_ms: i64,
    now: fn() -> i64,
    sessions: Mutex<HashMap<String, AcpSession>>,
    run_id_to_session_id: Mutex<HashMap<String, String>>,
}

impl InMemorySessionStore {
    fn new(options: AcpSessionStoreOptions) -> Self {
        let max_sessions = resolve_integer_option(
            options.max_sessions.map(|v| v as f64),
            DEFAULT_MAX_SESSIONS,
            1.0,
        );
        let idle_ttl_ms = resolve_integer_option(
            options.idle_ttl_ms.map(|v| v as f64),
            DEFAULT_IDLE_TTL_MS,
            1_000.0,
        );
        let now = options.now.unwrap_or(default_now);
        Self {
            max_sessions,
            idle_ttl_ms,
            now,
            sessions: Mutex::new(HashMap::new()),
            run_id_to_session_id: Mutex::new(HashMap::new()),
        }
    }

    fn touch_session(&self, session: &mut AcpSession, now_ms: i64) {
        session.last_touched_at = now_ms;
    }

    fn remove_session(&self, session_id: &str) -> bool {
        let mut sessions = self.sessions.lock().unwrap();
        let mut run_map = self.run_id_to_session_id.lock().unwrap();
        let session = match sessions.get_mut(session_id) {
            Some(s) => s,
            None => return false,
        };
        if let Some(active) = &session.active_run_id {
            run_map.remove(active);
        }
        if let Some(ac) = &mut session.abort_controller {
            ac.abort();
        }
        sessions.remove(session_id);
        true
    }

    fn reap_idle_sessions(&self, now_ms: i64) {
        let idle_before = now_ms - self.idle_ttl_ms;
        let mut to_remove: Vec<String> = vec![];
        {
            let sessions = self.sessions.lock().unwrap();
            for (session_id, session) in sessions.iter() {
                if session.active_run_id.is_some() || session.abort_controller.is_some() {
                    continue;
                }
                if session.last_touched_at > idle_before {
                    continue;
                }
                to_remove.push(session_id.clone());
            }
        }
        for id in to_remove {
            self.remove_session(&id);
        }
    }

    fn evict_oldest_idle_session(&self) -> bool {
        let oldest_session_id: Option<String> = {
            let sessions = self.sessions.lock().unwrap();
            sessions
                .iter()
                .filter(|(_, s)| s.active_run_id.is_none() && s.abort_controller.is_none())
                .min_by_key(|(_, s)| s.last_touched_at)
                .map(|(id, _)| id.clone())
        };
        match oldest_session_id {
            Some(id) => self.remove_session(&id),
            None => false,
        }
    }
}

fn default_now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

impl AcpSessionStore for InMemorySessionStore {
    fn create_session(&self, params: CreateSessionParams) -> AcpSession {
        let now_ms = (self.now)();
        let session_id = params
            .session_id
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(existing) = sessions.get_mut(&session_id) {
            existing.session_key = params.session_key;
            if let Some(lid) = params.ledger_session_id {
                existing.ledger_session_id = Some(lid);
            } else {
                existing.ledger_session_id = None;
            }
            existing.cwd = params.cwd;
            self.touch_session(existing, now_ms);
            return existing.clone();
        }
        // Active runs are never evicted; callers must clear/cancel them first.
        self.reap_idle_sessions(now_ms);
        if sessions.len() as i64 >= self.max_sessions && !self.evict_oldest_idle_session() {
            drop(sessions);
            panic!(
                "ACP session limit reached (max {}). Close idle ACP clients and retry.",
                self.max_sessions
            );
        }
        let session = AcpSession {
            session_id: session_id.clone(),
            session_key: params.session_key,
            ledger_session_id: params.ledger_session_id,
            cwd: params.cwd,
            created_at: now_ms,
            last_touched_at: now_ms,
            abort_controller: None,
            active_run_id: None,
        };
        sessions.insert(session_id, session.clone());
        session
    }

    fn has_session(&self, session_id: &str) -> bool {
        self.sessions.lock().unwrap().contains_key(session_id)
    }

    fn get_session(&self, session_id: &str) -> Option<AcpSession> {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(s) = sessions.get_mut(session_id) {
            self.touch_session(s, (self.now)());
            Some(s.clone())
        } else {
            None
        }
    }

    fn get_session_by_run_id(&self, run_id: &str) -> Option<AcpSession> {
        let run_map = self.run_id_to_session_id.lock().unwrap();
        let session_id = match run_map.get(run_id) {
            Some(id) => id.clone(),
            None => return None,
        };
        drop(run_map);
        self.get_session(&session_id)
    }

    fn set_active_run(&self, session_id: &str, run_id: &str, abort_controller: AbortController) {
        let mut sessions = self.sessions.lock().unwrap();
        let mut run_map = self.run_id_to_session_id.lock().unwrap();
        if let Some(session) = sessions.get_mut(session_id) {
            if let Some(active) = &session.active_run_id {
                if active != run_id {
                    run_map.remove(active);
                }
            }
            session.active_run_id = Some(run_id.to_string());
            session.abort_controller = Some(abort_controller);
            run_map.insert(run_id.to_string(), session_id.to_string());
            self.touch_session(session, (self.now)());
        }
    }

    fn clear_active_run(&self, session_id: &str) {
        let mut sessions = self.sessions.lock().unwrap();
        let mut run_map = self.run_id_to_session_id.lock().unwrap();
        if let Some(session) = sessions.get_mut(session_id) {
            if let Some(active) = &session.active_run_id {
                run_map.remove(active);
            }
            session.active_run_id = None;
            session.abort_controller = None;
            self.touch_session(session, (self.now)());
        }
    }

    fn cancel_active_run(&self, session_id: &str) -> bool {
        let mut sessions = self.sessions.lock().unwrap();
        let mut run_map = self.run_id_to_session_id.lock().unwrap();
        if let Some(session) = sessions.get_mut(session_id) {
            if session.abort_controller.is_none() {
                return false;
            }
            if let Some(ac) = session.abort_controller.as_mut() {
                ac.abort();
            }
            if let Some(active) = &session.active_run_id {
                run_map.remove(active);
            }
            session.abort_controller = None;
            session.active_run_id = None;
            self.touch_session(session, (self.now)());
            return true;
        }
        false
    }

    fn delete_session(&self, session_id: &str) -> bool {
        self.remove_session(session_id)
    }

    fn clear_all_sessions_for_test(&self) {
        let mut sessions = self.sessions.lock().unwrap();
        let mut run_map = self.run_id_to_session_id.lock().unwrap();
        for session in sessions.values_mut() {
            if let Some(ac) = session.abort_controller.as_mut() {
                ac.abort();
            }
        }
        sessions.clear();
        run_map.clear();
    }
}

/// Returns the default in-memory ACP session store (lazily initialized).
pub fn default_acp_session_store() -> std::sync::Arc<InMemorySessionStore> {
    use std::sync::OnceLock;
    static DEFAULT_STORE: OnceLock<std::sync::Arc<InMemorySessionStore>> = OnceLock::new();
    DEFAULT_STORE
        .get_or_init(|| std::sync::Arc::new(InMemorySessionStore::new(AcpSessionStoreOptions::default())))
        .clone()
}