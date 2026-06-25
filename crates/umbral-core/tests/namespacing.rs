//! Tests for plugin-namespaced table names (gap 30).
//!
//! Verifies that `#[umbral(plugin = "...")]` sets `Model::TABLE` to
//! `"<plugin>_<snake_case_struct>"`, that an explicit `table = "..."`
//! always wins regardless of whether `plugin` is also set, and that
//! models with neither attribute keep the bare snake_case name.

use umbral::orm::Model;

/// A model with only `plugin = "blog"` — table should be `"blog_post"`.
#[derive(Debug, Clone, sqlx::FromRow, umbral::orm::Model)]
#[umbral(plugin = "blog")]
pub struct Post {
    pub id: i64,
    pub title: String,
}

/// A model with both `plugin = "blog"` and an explicit `table = "custom_posts"`.
/// The explicit table wins.
#[derive(Debug, Clone, sqlx::FromRow, umbral::orm::Model)]
#[umbral(plugin = "blog", table = "custom_posts")]
pub struct FeaturedPost {
    pub id: i64,
    pub title: String,
}

/// A model with neither attribute — stays bare snake_case.
#[derive(Debug, Clone, sqlx::FromRow, umbral::orm::Model)]
pub struct Category {
    pub id: i64,
    pub name: String,
}

/// A model with explicit `table` but no `plugin` — still uses the explicit name.
#[derive(Debug, Clone, sqlx::FromRow, umbral::orm::Model)]
#[umbral(table = "auth_user")]
pub struct User {
    pub id: i64,
    pub username: String,
}

#[test]
fn plugin_attribute_produces_namespaced_table() {
    assert_eq!(
        <Post as Model>::TABLE,
        "blog_post",
        "plugin = \"blog\" on struct Post should produce table name \"blog_post\""
    );
}

#[test]
fn explicit_table_wins_over_plugin_prefix() {
    assert_eq!(
        <FeaturedPost as Model>::TABLE,
        "custom_posts",
        "explicit table = \"custom_posts\" should win over plugin = \"blog\" prefix"
    );
}

#[test]
fn no_attribute_stays_bare_snake_case() {
    assert_eq!(
        <Category as Model>::TABLE,
        "category",
        "a model with no umbral attribute should use bare snake_case struct name"
    );
}

#[test]
fn explicit_table_without_plugin_unchanged() {
    assert_eq!(
        <User as Model>::TABLE,
        "auth_user",
        "explicit table without plugin attribute should be the explicit name"
    );
}

#[test]
fn plugin_attribute_does_not_change_model_name() {
    // Model::NAME is the Rust struct name — the rename-detection anchor.
    // It must NOT be affected by the plugin attribute.
    assert_eq!(
        <Post as Model>::NAME,
        "Post",
        "Model::NAME must remain the Rust struct name regardless of plugin attribute"
    );
}

/// A struct whose snake_case expansion has multiple words — used in the
/// `plugin_prefix_multiword_struct` test below.
#[derive(Debug, Clone, sqlx::FromRow, umbral::orm::Model)]
#[umbral(plugin = "shop")]
pub struct OrderItem {
    pub id: i64,
    pub quantity: i32,
}

#[test]
fn plugin_prefix_multiword_struct() {
    assert_eq!(
        <OrderItem as Model>::TABLE,
        "shop_order_item",
        "plugin = \"shop\" on struct OrderItem should produce \"shop_order_item\""
    );
}
