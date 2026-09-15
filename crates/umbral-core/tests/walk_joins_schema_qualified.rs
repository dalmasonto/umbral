//! ORM heavy-relations epic, Plan A Task 3 (SECURITY) — `walk_joins` must
//! schema-qualify EVERY joined table (root, every intermediate hop, a
//! reverse-FK child, and an M2M junction), not just the query's own root. A
//! miss here crosses a tenant boundary under the schema-per-tenant router.
//!
//! Mirrors `router_schema_qualified.rs`'s pattern (an installed
//! `DatabaseRouter::schema_for` returning `Some(schema)` makes every table
//! position dot-qualified) but drives it through `RelPath::from_path` +
//! `walk_joins`, so the qualifier is proven for a real JOIN target, not just
//! a query's own FROM. Three hop kinds are exercised end-to-end here:
//!
//! - `walk_joins_schema_qualifies_every_hop` — a genuine 2-hop forward-FK
//!   chain (`Post.author -> User`, `User.company -> Company`), via the
//!   `to_sql_for_path` probe (`build_to_one_select`).
//! - `walk_joins_schema_qualifies_m2m_junction_and_target` — an M2M hop
//!   (`Post.tags -> Tag`), via `.join_related("tags").to_sql()`
//!   (`apply_join_related`), asserting BOTH the junction table AND the
//!   target table are schema-qualified.
//! - `walk_joins_schema_qualifies_reverse_fk_child` — a reverse-FK hop
//!   (`Company` <- `User` via `User.company`), via
//!   `.join_related("user_set").to_sql()`, asserting the child table is
//!   schema-qualified.
//!
//! SQL-string assertions are the only viable proof here (never a bogus
//! round-trip): schema-per-tenant isn't meaningfully round-trippable on
//! SQLite, which has no schemas — matching this file's (and
//! `router_schema_qualified.rs`'s) existing precedent.
//!
//! Kept in its own process (own test binary): the schema router and the
//! model registry are both process-wide `OnceLock`s, so installing a
//! `SchemaRouter` here must not run in the same binary as the
//! `DefaultRouter` assertions in `walk_joins_sql.rs`.

use tokio::sync::OnceCell;
use umbral::db::{DatabaseRouter, RouteContext, Schema};
use umbral::orm::{ForeignKey, M2M, Model};

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "wjq_company")]
pub struct Company {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "wjq_user")]
pub struct User {
    pub id: i64,
    pub name: String,
    pub company: ForeignKey<Company>,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "wjq_tag")]
pub struct Tag {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "wjq_post")]
pub struct Post {
    pub id: i64,
    pub title: String,
    pub author: ForeignKey<User>,
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbral(m2m = "wjq_tag")]
    pub tags: M2M<Tag>,
}

/// Unconditionally scopes every request to schema `tenant1`, ignoring ctx.
struct SchemaRouter;
impl DatabaseRouter for SchemaRouter {
    fn schema_for(&self, _ctx: &RouteContext) -> Option<Schema> {
        Some(Schema::new("tenant1").expect("valid schema identifier"))
    }
}

/// Shared boot, run exactly once for every test in this file — `App::build`
/// publishes the model registry and the `SchemaRouter` into process-wide
/// `OnceLock`s, so a second `build()` call in the same process would either
/// panic or leave the first registration in place. Every test needs the
/// SAME registered set (`Company`/`User`/`Tag`/`Post`, `Post` carrying the
/// M2M `tags` field) to resolve both the forward-FK chain and the new M2M /
/// reverse-FK hops.
static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let pool = umbral_core::db::connect_sqlite("sqlite::memory:")
            .await
            .unwrap();
        umbral::App::builder()
            .settings(umbral::Settings::from_env().expect("settings load"))
            .database("default", pool)
            .router(SchemaRouter)
            .model::<Company>()
            .model::<User>()
            .model::<Tag>()
            .model::<Post>()
            .build()
            .expect("App::build");
        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn walk_joins_schema_qualifies_every_hop() {
    boot().await;

    use umbral::orm::relation::RelPath;

    let path = RelPath::from_path::<Post>("author__company").unwrap();
    let sql = umbral::orm::relation::to_sql_for_path::<Company>(&path).unwrap();

    // Root (post) + hop 1 (user) + hop 2 (company) = 3 schema-qualified
    // table references, every one under "tenant1".
    assert_eq!(
        sql.matches("\"tenant1\"").count(),
        3,
        "root + 2 hops all schema-qualified under tenant1: {sql}"
    );
    assert!(
        sql.contains(&format!("\"tenant1\".\"{}\"", Post::TABLE)),
        "root not schema-qualified: {sql}"
    );
    assert!(
        sql.contains(&format!("\"tenant1\".\"{}\"", User::TABLE)),
        "hop 1 (user) not schema-qualified: {sql}"
    );
    assert!(
        sql.contains(&format!("\"tenant1\".\"{}\"", Company::TABLE)),
        "hop 2 (company) not schema-qualified: {sql}"
    );
}

/// The M2M arm (multi-tenant isolation proof): `Post.tags -> Tag` walks
/// through `walk_joins`'s `HopKind::M2M` branch, which emits TWO physical
/// joins (parent -> junction, junction -> target) — both must be
/// schema-qualified, or a tenant's query would silently read another
/// tenant's junction rows (and, through them, another tenant's `Tag` rows).
/// Driven through `.join_related("tags").to_sql()` (`apply_join_related`),
/// the same walk_joins-unified code path `join_related_m2m.rs` exercises
/// without a schema router — this is that same M2M JOIN shape, now proven
/// under an installed tenant router.
#[tokio::test(flavor = "multi_thread")]
async fn walk_joins_schema_qualifies_m2m_junction_and_target() {
    boot().await;

    let sql = Post::objects().join_related("tags").to_sql();

    // The junction table name follows the derive's own convention
    // (`<parent_table>_<field_name>`; see `RelPath::from_path`'s M2M arm),
    // computed here rather than hard-coded so this test breaks loudly if
    // that convention ever changes instead of silently asserting a stale
    // literal.
    let junction_table = format!("{}_tags", Post::TABLE);

    // Root (post, inside the wrapped subquery) + M2M junction + M2M target
    // (tag) = 3 schema-qualified table references, every one under
    // "tenant1". Counting (not just asserting presence) means a missed
    // table — e.g. the junction qualified but the target left bare — fails
    // this test instead of passing on a partial fix.
    assert_eq!(
        sql.matches("\"tenant1\"").count(),
        3,
        "root + M2M junction + M2M target all schema-qualified under tenant1: {sql}"
    );
    assert!(
        sql.contains(&format!("\"tenant1\".\"{}\"", Post::TABLE)),
        "root not schema-qualified: {sql}"
    );
    assert!(
        sql.contains(&format!("\"tenant1\".\"{junction_table}\"")),
        "M2M junction table not schema-qualified — a cross-tenant leak \
         surface (another tenant's link rows would be readable): {sql}"
    );
    assert!(
        sql.contains(&format!("\"tenant1\".\"{}\"", Tag::TABLE)),
        "M2M target table not schema-qualified — a cross-tenant leak \
         surface: {sql}"
    );
}

/// The reverse-FK arm (multi-tenant isolation proof): `Company` <- `User`
/// (auto-discovered via `User.company`, the same convention
/// `RelPath::from_path`'s reverse-FK arm documents: bare table name /
/// struct name in snake_case, or either with a `_set` suffix) walks through
/// `walk_joins`'s `HopKind::ReverseFk` branch. Driven through
/// `.join_related("user_set").to_sql()`, proving the child table is
/// schema-qualified under an installed tenant router — a miss here would
/// let one tenant's query read another tenant's child rows straight off
/// the reverse relation.
#[tokio::test(flavor = "multi_thread")]
async fn walk_joins_schema_qualifies_reverse_fk_child() {
    boot().await;

    let sql = Company::objects().join_related("user_set").to_sql();

    // Root (company, inside the wrapped subquery) + reverse-FK child
    // (user) = 2 schema-qualified table references, every one under
    // "tenant1".
    assert_eq!(
        sql.matches("\"tenant1\"").count(),
        2,
        "root + reverse-FK child both schema-qualified under tenant1: {sql}"
    );
    assert!(
        sql.contains(&format!("\"tenant1\".\"{}\"", Company::TABLE)),
        "root not schema-qualified: {sql}"
    );
    assert!(
        sql.contains(&format!("\"tenant1\".\"{}\"", User::TABLE)),
        "reverse-FK child table not schema-qualified — a cross-tenant leak \
         surface: {sql}"
    );
}
