//! my-api library
//!
//! This is the main library crate for my-api.

pub mod config;
pub mod apps;

// Re-export commonly used items
pub use config::settings::get_settings;
pub use config::urls::routes;
