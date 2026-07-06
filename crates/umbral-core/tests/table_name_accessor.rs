#![allow(dead_code, private_interfaces)]

//! gaps.md — ergonomic table-name accessor. A model's SQL table name can
//! diverge from its struct name (`UserProfile` → `profile`), and hardcoding the
//! string literal is error-prone. `Model::table_name()` returns it as a call so
//! consumers never type the literal or the `<T as Model>::TABLE` turbofish.

use umbral::orm::Model;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "profile")]
struct UserProfile {
    id: i64,
    display_name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
struct Comment {
    id: i64,
    body: String,
}

#[test]
fn table_name_returns_the_sql_table_not_the_struct_name() {
    // Overridden table name — the whole point of the accessor.
    assert_eq!(UserProfile::table_name(), "profile");
    // It equals the TABLE const it wraps.
    assert_eq!(UserProfile::table_name(), <UserProfile as Model>::TABLE);
    // Default (snake_case of struct) still works.
    assert_eq!(Comment::table_name(), "comment");
}
