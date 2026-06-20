//! Request-scoped session holder (`RequestSession`) + the `CURRENT_SESSION`
//! task-local that `session_layer` scopes around each request.
//!
//! ## Design
//!
//! `session_layer` loads the session record at request entry (one `load`
//! against the [`SessionStore`]), parks a `RequestSession` in the
//! `CURRENT_SESSION` task-local for the duration of the handler, and — at
//! exit — persists the record back via the store ONLY if the handler
//! mutated it (`dirty`). This reproduces Django's lazy-creation contract
//! (gaps2 #46): a request that never writes the session leaves zero rows
//! and sets no cookie.
//!
//! The holder carries the *raw* token (the cookie value), the `fresh`
//! flag (true when no live row existed on entry — the candidate token was
//! minted in memory), the in-memory [`SessionRecord`] (`None` until a row
//! exists / is materialised), and a `dirty` flag flipped by any mutator.
//!
//! Reads go through [`current`]; mutations through [`current_mut`]. Both
//! return `None` outside a request scope (e.g. a background task), so a
//! caller can degrade gracefully rather than panic.

use std::cell::RefCell;

use chrono::{Duration, Utc};

use crate::store::SessionRecord;
use crate::DEFAULT_TTL_SECONDS;

tokio::task_local! {
    /// The session for the in-flight request. `session_layer` scopes this
    /// around the handler future; [`current`] / [`current_mut`] read it.
    ///
    /// `RefCell` because a handler reads and mutates through a shared
    /// reference to the task-local value; the borrow is request-local and
    /// single-threaded (one task owns the scope), so the `RefCell` never
    /// contends.
    pub(crate) static CURRENT_SESSION: RefCell<RequestSession>;
}

/// The session bound to the current request.
///
/// Materialised lazily: `record` stays `None` until either a live row was
/// loaded on entry or a mutator (`set_raw` / `rotate`) creates one. The
/// `dirty` flag is what `session_layer` consults at exit to decide whether
/// to call `store.save(...)` — and, for a `fresh` session, whether to emit
/// the `Set-Cookie`.
#[derive(Debug, Clone)]
pub struct RequestSession {
    /// The raw cookie token (NOT the hashed stored id). The store hashes
    /// it before any DB access.
    pub(crate) token: String,
    /// True when no live row existed on entry (cookie absent / stale /
    /// expired) and the token was minted in memory. A `fresh` session that
    /// stays clean leaves no row and no cookie.
    pub(crate) fresh: bool,
    /// The in-memory record. `None` until a row exists (loaded) or is
    /// materialised by a write.
    pub(crate) record: Option<SessionRecord>,
    /// Flipped by any mutator. Drives the exit-time `save`.
    pub(crate) dirty: bool,
}

impl RequestSession {
    /// Construct from the pieces `session_layer` resolves on entry.
    pub(crate) fn new(token: String, fresh: bool, record: Option<SessionRecord>) -> Self {
        Self {
            token,
            fresh,
            record,
            dirty: false,
        }
    }

    /// The raw session token (the cookie value).
    pub fn token(&self) -> &str {
        &self.token
    }

    /// True when this request will need to issue a fresh cookie if it
    /// writes (no live row existed on entry).
    pub fn is_fresh(&self) -> bool {
        self.fresh
    }

    /// True when a mutator has touched the record this request.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// The record this request will persist at exit, if any.
    pub(crate) fn record(&self) -> Option<&SessionRecord> {
        self.record.as_ref()
    }

    /// The user PK string stashed on the loaded record, if any.
    pub fn user_id(&self) -> Option<&str> {
        self.record.as_ref().and_then(|r| r.user_id.as_deref())
    }

    /// Read one key out of the in-memory record's JSON `data` map.
    /// Returns `None` if there's no record yet, the data is malformed, or
    /// the key is absent.
    pub fn get_raw(&self, key: &str) -> Option<serde_json::Value> {
        let record = self.record.as_ref()?;
        let map: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&record.data).ok()?;
        map.get(key).cloned()
    }

    /// Materialise the record if absent, then write `key = val` into its
    /// JSON `data` map and mark the session dirty. A fresh materialised
    /// record is anonymous (`user_id = None`) with the default 14-day TTL,
    /// matching `create_session(None, None)`.
    pub fn set_raw(&mut self, key: &str, val: serde_json::Value) {
        let record = self.ensure_record();
        let mut map: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&record.data).unwrap_or_default();
        map.insert(key.to_string(), val);
        record.data = serde_json::Value::Object(map).to_string();
        self.dirty = true;
    }

    /// Sliding expiry: extend the loaded record's `expires_at` and mark
    /// the session dirty so `session_layer`'s exit save persists the new
    /// window. No-op if no record was loaded (a fresh session never gets a
    /// sliding bump — the lazy-creation contract stays intact). Sharing the
    /// exit save with any `set_raw` write this request is what keeps the
    /// bump from being clobbered by the save of a stale `expires_at`.
    pub fn bump_expiry(&mut self, new_expires: chrono::DateTime<Utc>) {
        if let Some(record) = self.record.as_mut() {
            record.expires_at = new_expires;
            self.dirty = true;
        }
    }

    /// Login rotation: mint a NEW token and a fresh record pinned to
    /// `user_id`, optionally carrying the old record's `data` string over
    /// (flash messages, cart, etc.). Marks the session dirty and fresh so
    /// the exit path issues a new cookie.
    ///
    /// This is the in-memory analogue of `login_user_id`'s
    /// destroy-old + create-new + carry-data flow. The OLD row's
    /// destruction is the caller's responsibility (the layer / login
    /// helper), since `RequestSession` only owns the in-memory state.
    pub fn rotate(&mut self, user_id: Option<String>, carry_data: bool) {
        let carried = if carry_data {
            self.record.as_ref().map(|r| r.data.clone())
        } else {
            None
        };
        let now = Utc::now();
        self.token = uuid::Uuid::new_v4().to_string();
        self.fresh = true;
        self.record = Some(SessionRecord {
            user_id,
            data: carried.filter(|d| d != "{}").unwrap_or_else(|| "{}".to_string()),
            created_at: now,
            expires_at: now + Duration::seconds(DEFAULT_TTL_SECONDS),
        });
        self.dirty = true;
    }

    /// Return a mutable reference to the record, creating an empty
    /// anonymous one (14-day TTL) if none exists yet.
    fn ensure_record(&mut self) -> &mut SessionRecord {
        if self.record.is_none() {
            let now = Utc::now();
            self.record = Some(SessionRecord {
                user_id: None,
                data: "{}".to_string(),
                created_at: now,
                expires_at: now + Duration::seconds(DEFAULT_TTL_SECONDS),
            });
        }
        self.record.as_mut().expect("record just materialised")
    }
}

/// Run `f` against the current request's session, returning `Some(result)`
/// when called inside a request scope and `None` otherwise (e.g. a
/// background task with no session).
pub fn current<R>(f: impl FnOnce(&RequestSession) -> R) -> Option<R> {
    CURRENT_SESSION
        .try_with(|cell| f(&cell.borrow()))
        .ok()
}

/// Run `f` against a mutable view of the current request's session.
/// Returns `None` outside a request scope. Any mutation that flips the
/// `dirty` flag will be persisted by `session_layer` at request exit.
pub fn current_mut<R>(f: impl FnOnce(&mut RequestSession) -> R) -> Option<R> {
    CURRENT_SESSION
        .try_with(|cell| f(&mut cell.borrow_mut()))
        .ok()
}
