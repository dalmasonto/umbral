//! Behavioral coverage for M2M form fields. The Form derive classifies
//! `M2M<T>` into a `ModelMultiChoice`: validate() parses the id list,
//! verifies each id exists (one batched query), and stages the ids on
//! the M2M field's pending slot. The typed create() flushes them to the
//! junction table after the parent insert. Tests read the junction
//! table directly and assert exactly the selected rows + atomicity.

#![allow(dead_code)]
use std::collections::HashMap;
use tokio::sync::OnceCell;
use umbral::forms::FormValidate;
use umbral_core::db;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "fm_tag")]
struct Tag {
    pub id: i64,
    pub name: String,
}

/// A model that's REGISTERED (so `validate_multi_fk_exists` gets past the
/// registry lookup) but whose backing table is deliberately never created.
/// Used to drive the DB-error path: the existence query fails with
/// "no such table", which must NOT be silently swallowed into a flood of
/// per-field "no matching record" errors (gaps2 #48).
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "fm_phantom")]
struct Phantom {
    pub id: i64,
    pub name: String,
}

#[derive(
    Debug,
    Clone,
    Default,
    sqlx::FromRow,
    serde::Serialize,
    serde::Deserialize,
    umbral::orm::Model,
    umbral::forms::Form,
)]
#[umbral(table = "fm_article")]
struct Article {
    #[umbral(primary_key)]
    pub id: i64,
    #[form(required, length(min = 1, max = 200))]
    pub title: String,
    #[sqlx(skip)]
    #[serde(skip)]
    pub tags: umbral::orm::M2M<Tag>,
}

fn data(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

// Multi-value submission: the form layer collapses repeated keys into a
// comma-joined string under the field name. Here we feed the joined form
// directly.
fn data_multi(title: &str, tag_ids: &[&str]) -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("title".to_string(), title.to_string());
    m.insert("tags".to_string(), tag_ids.join(","));
    m
}

static BOOT: OnceCell<()> = OnceCell::const_new();
async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let pool = db::connect_sqlite("sqlite::memory:").await.expect("sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Tag>()
            .model::<Article>()
            // Registered but its table is never created — drives the
            // DB-error path in the gaps2 #48 test below.
            .model::<Phantom>()
            .build()
            .expect("App::build");

        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        // `fm_phantom` is REGISTERED but must have NO table: that is what
        // `m2m_validation_db_error_is_non_field_not_per_id_misses` exercises — a real
        // database error ("no such table") has to surface as a non-field error, not as N
        // bogus "no matching record" errors keyed to the field. The derived schema creates
        // a table for every registered model, so the absence has to be arranged on purpose.
        sqlx::query("DROP TABLE fm_phantom")
            .execute(&pool)
            .await
            .expect("drop fm_phantom so it is registered-but-missing");
        // Junction follows the <parent_table>_<field> convention.
        // Six tags: the junction round-trip below sets {1,2,3} → {2,3,4} → {5,5,6}, and
        // every one of those child ids must be a real row now that the junction carries the
        // FOREIGN KEY the migration engine emits.
        for name in ["a", "b", "c", "d", "e", "f"] {
            sqlx::query("INSERT INTO fm_tag (name) VALUES (?)")
                .bind(name)
                .execute(&pool)
                .await
                .expect("seed tag");
        }
    })
    .await;
}

async fn junction_child_ids(parent_id: i64) -> Vec<i64> {
    let pool = db::pool();
    let rows: Vec<(i64,)> = sqlx::query_as(
        "SELECT child_id FROM fm_article_tags WHERE parent_id = ? ORDER BY child_id",
    )
    .bind(parent_id)
    .fetch_all(&pool)
    .await
    .expect("read junction");
    rows.into_iter().map(|(c,)| c).collect()
}

#[tokio::test]
async fn m2m_form_writes_exactly_the_selected_junction_rows() {
    boot().await;
    let article = Article::validate(&data_multi("Intro", &["1", "2"]))
        .await
        .expect("valid m2m");
    let created = Article::objects()
        .create(article)
        .await
        .expect("create article");
    let ids = junction_child_ids(created.id).await;
    assert_eq!(
        ids,
        vec![1, 2],
        "exactly the two selected tags are junction rows (3rd absent)"
    );
}

#[tokio::test]
async fn m2m_form_bad_id_writes_zero_junction_rows() {
    boot().await;
    // A single bad id fails validation BEFORE any insert runs. The
    // returned ValidationErrors never reaches create(), so the parent
    // (with this unique title) is never inserted and no junction row is
    // written. Asserting on the specific title avoids racing with the
    // other test's insert into the shared in-memory DB.
    let err = Article::validate(&data_multi("Broken-unique-title", &["1", "9999"]))
        .await
        .expect_err("bad id rejected");
    assert!(
        err.fields.contains_key("tags"),
        "error keyed to the m2m field"
    );
    // No parent row with this title exists — validation short-circuited
    // before create() ever ran.
    let count = Article::objects()
        .filter(article::TITLE.eq("Broken-unique-title"))
        .count()
        .await
        .expect("count by title");
    assert_eq!(count, 0, "no parent row inserted on a bad m2m id");
}

// gaps2 #48 — a real DB error during M2M reference validation must NOT
// masquerade as N per-field "id has no matching record" errors. The
// helper used to `.unwrap_or_default()` the query result, turning any DB
// failure into an empty result set so every submitted id was then flagged
// "not found" — a bogus, per-field error that hid the real failure. The
// fix: log the error and surface ONE honest non-field error, leaving the
// per-field bucket untouched (fail-closed but honest).
#[tokio::test]
async fn m2m_validation_db_error_is_non_field_not_per_id_misses() {
    boot().await;
    // `fm_phantom` is registered but its table was never created, so the
    // `SELECT ... WHERE id IN (...)` existence query fails with
    // "no such table". Feed several ids: a swallow-into-empty bug would
    // produce one per-field error PER id.
    let ids: Vec<String> = vec!["1".into(), "2".into(), "3".into()];
    let mut errs = umbral::forms::ValidationErrors::new();
    let out = umbral::orm::forms_runtime::validate_multi_fk_exists(
        "phantomtags",
        &ids,
        "fm_phantom",
        &mut errs,
    )
    .await;

    // No staged ids on a failed validation.
    assert!(out.is_empty(), "no ids staged when the query failed");
    // CRITICAL: the field bucket is empty — NOT N bogus
    // "no matching record" errors keyed to the field.
    assert!(
        !errs.fields.contains_key("phantomtags"),
        "DB error must NOT be flagged as per-field id misses: {:?}",
        errs.fields
    );
    // The failure surfaces as exactly one honest non-field error.
    assert_eq!(
        errs.non_field.len(),
        1,
        "one non-field system error, got: {:?}",
        errs.non_field
    );
    assert!(
        errs.non_field[0].contains("database error"),
        "non-field error names the database failure: {:?}",
        errs.non_field[0]
    );
}

// gaps2 #48 — the genuine-miss path (query succeeds, a truly-absent id)
// must STILL produce the per-field "no matching record" error. The fix
// for the DB-error path must not weaken this honest validation.
#[tokio::test]
async fn m2m_validation_genuine_miss_still_per_field() {
    boot().await;
    // Tags 1..3 exist; 9999 does not. The query runs fine; the missing id
    // is a real user error and belongs in the per-field bucket.
    let ids: Vec<String> = vec!["1".into(), "9999".into()];
    let mut errs = umbral::forms::ValidationErrors::new();
    let out =
        umbral::orm::forms_runtime::validate_multi_fk_exists("tags", &ids, "fm_tag", &mut errs)
            .await;

    // The one valid id is staged.
    assert_eq!(out.len(), 1, "the valid id (1) is staged");
    // The missing id is a per-field error, not a non-field system error.
    assert!(
        errs.fields.contains_key("tags"),
        "genuine miss is keyed to the field: {:?}",
        errs.fields
    );
    assert!(
        errs.fields["tags"][0].contains("9999"),
        "per-field error names the missing id: {:?}",
        errs.fields["tags"]
    );
    assert!(
        errs.non_field.is_empty(),
        "a genuine miss is NOT a system error: {:?}",
        errs.non_field
    );
}

// gaps2 #47 — set_junction_dynamic batches the junction write into ONE
// multi-row INSERT (was M per-id INSERTs in a loop). Behavioral: the
// DELETE+reinsert semantics and ON CONFLICT DO NOTHING must be preserved.
// Uses a high, otherwise-untouched parent_id so it doesn't collide with
// the other tests sharing this in-memory junction table.
fn bigint(n: i64) -> umbral::_sea_query::Value {
    umbral::_sea_query::Value::BigInt(Some(n))
}

#[tokio::test]
async fn set_junction_dynamic_batched_insert_round_trip() {
    boot().await;
    let parent = 777_000_i64;
    let pval = bigint(parent);

    // The parent row has to exist. The junction the migration engine emits carries
    // `parent_id REFERENCES fm_article(id)`, so a junction row for a parent that was
    // never created is refused — as it would be in production. The old hand-written
    // junction had no FK, which is why this test could invent an id out of thin air.
    sqlx::query("INSERT INTO fm_article (id, title) VALUES (?, ?)")
        .bind(parent)
        .bind("junction parent")
        .execute(&umbral::db::pool())
        .await
        .expect("seed the parent article");

    // Set {1,2,3} → exactly those three rows exist.
    umbral::orm::set_junction_dynamic(
        "fm_article_tags",
        pval.clone(),
        vec![bigint(1), bigint(2), bigint(3)],
        None,
    )
    .await
    .expect("set {1,2,3}");
    assert_eq!(
        junction_child_ids(parent).await,
        vec![1, 2, 3],
        "batched insert writes exactly the three selected child ids"
    );

    // Re-set {2,3,4} → DELETE+reinsert: 1 drops, 4 appears, holds {2,3,4}.
    umbral::orm::set_junction_dynamic(
        "fm_article_tags",
        pval.clone(),
        vec![bigint(2), bigint(3), bigint(4)],
        None,
    )
    .await
    .expect("re-set {2,3,4}");
    assert_eq!(
        junction_child_ids(parent).await,
        vec![2, 3, 4],
        "DELETE-then-reinsert replaces the whole set (1 gone, 4 added)"
    );

    // A duplicate-containing list: ON CONFLICT DO NOTHING keeps it clean —
    // no duplicate rows, no error, even with a within-batch duplicate.
    umbral::orm::set_junction_dynamic(
        "fm_article_tags",
        pval.clone(),
        vec![bigint(5), bigint(5), bigint(6)],
        None,
    )
    .await
    .expect("set with an in-batch duplicate must not error");
    assert_eq!(
        junction_child_ids(parent).await,
        vec![5, 6],
        "ON CONFLICT DO NOTHING dedupes the in-batch duplicate to one row each"
    );

    // Empty list clears the relation (no INSERT emitted at all).
    umbral::orm::set_junction_dynamic("fm_article_tags", pval, Vec::new(), None)
        .await
        .expect("empty selection clears");
    assert!(
        junction_child_ids(parent).await.is_empty(),
        "empty child_ids clears the junction with no INSERT"
    );
}
