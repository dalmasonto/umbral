//! Background tasks — the shop's queue-owned work.
//!
//! Every `#[umbral::task]` here is DISCOVERED automatically (gaps4 #40):
//! the attribute registers the handler in a link-time slice and the
//! worker installs it at startup — nothing to wire in `main.rs`. Run the
//! worker alongside the server:
//!
//! ```text
//! cargo run -- tasks-worker
//! ```
//!
//! Enqueue with the typed handle the attribute generates
//! (`NotifyContactTask::enqueue(payload)`), so a payload-shape change is
//! a compile error at the enqueue site, not a runtime deserialise
//! failure in the worker.

use serde::{Deserialize, Serialize};

/// Payload for [`notify_contact_task`].
#[derive(Serialize, Deserialize)]
pub struct NotifyContactPayload {
    pub contact_id: i64,
    pub email: String,
}

/// Follow up on a submitted contact message OFF the request path: the
/// form handler stays a fast insert + redirect, and the slow part
/// (notify staff, send the acknowledgement email) happens in the worker
/// with retries for free. This demo logs; a real shop would render and
/// send mail here.
#[umbral::task]
pub async fn notify_contact_task(payload: NotifyContactPayload) -> Result<(), String> {
    tracing::info!(
        contact_id = payload.contact_id,
        email = %payload.email,
        "contact message received — notifying staff (demo: log only)"
    );
    Ok(())
}
