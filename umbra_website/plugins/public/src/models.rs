//! Models for the `public` plugin.
//!
//! Declare one `#[derive(umbra::orm::Model)]` struct per database
//! table. Once registered via `Plugin::models()` in lib.rs, the
//! migration engine picks them up on the next `makemigrations`.
//!
//! ```ignore
//! use chrono::{DateTime, Utc};
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
//! pub struct Example {
//!     pub id: i64,
//!     #[umbra(string, max_length = 200)]
//!     pub title: String,
//!     #[umbra(noedit)]
//!     pub created_at: DateTime<Utc>,
//! }
//! ```
