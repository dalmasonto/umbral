//! A plugin's own models get their tables from `boot`, without the test listing them.
//!
//! This is the claim that makes the helper worth having: `auth_user` and its friends are
//! registered by the plugin, so the schema derived from the registry includes them. Test
//! files no longer hand-write a `CREATE TABLE auth_user (...)` that goes stale the next
//! time the auth plugin grows a column.

use umbral_auth::{AuthPlugin, AuthUser};

async fn boot() {
    umbral_testing::boot(|b| b.plugin(AuthPlugin::<AuthUser>::default())).await;
}

#[tokio::test]
async fn a_plugins_tables_are_created_without_being_listed() {
    boot().await;

    // Nothing in this file mentions `auth_user`'s columns — the plugin declared them.
    let user = umbral_auth::create_user_with_flags("alice", "alice@example.com", "pw", true, true)
        .await
        .expect("the auth_user table exists, with every column the plugin's model declares");

    assert_eq!(user.username, "alice");
    assert!(user.is_staff);

    // And it is a real table the ORM can query back through.
    let found = AuthUser::objects()
        .filter(umbral_auth::auth_user::USERNAME.eq("alice"))
        .first()
        .await
        .expect("query")
        .expect("the row round-trips");
    assert_eq!(found.email, "alice@example.com");
}
