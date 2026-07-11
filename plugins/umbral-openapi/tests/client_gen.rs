//! gaps3 #38 / Kikosi #1 — the typed TypeScript query client.
//!
//! Drives `client_gen::generate_for` with real `#[derive(Model)]` types (so the
//! derive → registry → generator path is under test) and asserts the structural
//! guarantees a frontend relies on: a `Filters` type per model whose keys are
//! exactly the REST-filterable (column, lookup) pairs, whose value types match
//! the field types, with the primary key excluded — and the `Umbral` client that
//! keys `.from(...)` off the exposed tables.
//!
//! The generator reads `umbral_rest` config via `OnceLock`s that default
//! gracefully when unset (exposed, filters on, base path `/api`, no pagination),
//! so no `App::build` is needed here.
//!
//! `tests/client_gen_tsc.rs` compiles the emitted output with a real `tsc` to
//! prove the types actually constrain a consumer.

use serde::{Deserialize, Serialize};
use umbral::migrate::ModelMeta;

#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, umbral::orm::Choices, Serialize, Deserialize,
)]
#[choices(rename_all = "lowercase")]
pub enum CgStatus {
    #[default]
    Draft,
    Published,
    Archived,
}

/// String primary key on purpose: a foreign key to this model must type its
/// filter value as `string`, not `number` — the case a hand-written client
/// gets wrong.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "cg_author")]
pub struct CgAuthor {
    #[umbral(primary_key)]
    pub slug: String,
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "cg_post")]
pub struct CgPost {
    pub id: i64,
    pub title: String,
    pub body: Option<String>,
    #[umbral(choices)]
    pub status: CgStatus,
    pub views: i32,
    #[umbral(no_reverse)]
    pub author: umbral::orm::ForeignKey<CgAuthor>,
    /// Set on create, never editable — must be in Create but not Update.
    #[umbral(noedit)]
    pub slug: String,
    /// Never on any form — must be in neither Create nor Update.
    #[umbral(noform)]
    pub internal: String,
    /// Server-stamped — must be in neither write DTO.
    #[umbral(auto_now_add)]
    pub created_at: chrono::DateTime<chrono::Utc>,
}

fn client() -> String {
    let (_models, client) = umbral_openapi::client_gen::generate_for(&[
        ModelMeta::for_::<CgAuthor>(),
        ModelMeta::for_::<CgPost>(),
    ]);
    client
}

fn models() -> String {
    umbral_openapi::client_gen::generate_for(&[
        ModelMeta::for_::<CgAuthor>(),
        ModelMeta::for_::<CgPost>(),
    ])
    .0
}

#[track_caller]
fn assert_has(haystack: &str, needle: &str) {
    assert!(
        haystack.contains(needle),
        "expected to find:\n  {needle}\nin:\n{haystack}",
    );
}

/// The types file is exactly the typegen output for the exposed models, so it
/// carries the row interfaces and choice unions the client imports.
#[test]
fn models_file_has_the_row_types() {
    let m = models();
    assert_has(&m, "export interface CgPost {");
    assert_has(
        &m,
        r#"export type CgPostStatus = "draft" | "published" | "archived";"#,
    );
    assert_has(&m, "export interface CgAuthor {");
}

/// The headline feature: a per-model filter type whose keys are the exact
/// query-params the REST list endpoint accepts, typed to the field's type.
#[test]
fn filters_type_lists_every_lookup_typed_to_the_field() {
    let c = client();
    assert_has(&c, "export interface CgPostFilters {");

    // choices column → the enum union, plus __ne and __in (array of it).
    assert_has(&c, r#"  "status"?: CgPostStatus;"#);
    assert_has(&c, r#"  "status__in"?: CgPostStatus[];"#);

    // numeric column → number, with the comparison lookups.
    assert_has(&c, r#"  "views__gte"?: number;"#);
    assert_has(&c, r#"  "views__lt"?: number;"#);

    // text column → substring lookups are string.
    assert_has(&c, r#"  "title__contains"?: string;"#);

    // nullable column → __isnull is boolean.
    assert_has(&c, r#"  "body__isnull"?: boolean;"#);
}

/// The reason to generate rather than hand-write: an FK filter value is the
/// target's PK type. `author` points at a String-PK model, so it's `string`.
#[test]
fn foreign_key_filter_value_is_the_targets_pk_type() {
    let c = client();
    assert_has(&c, r#"  "author"?: string;"#);
    assert!(
        !c.contains(r#"  "author"?: number;"#),
        "an FK to a String-PK model must filter by string, not number:\n{c}",
    );
}

/// The primary key is not a filter parameter (matches the REST contract, which
/// excludes the PK from filter params).
#[test]
fn primary_key_is_not_filterable() {
    let c = client();
    // `id` must not appear as a filter key on CgPost.
    let filters = c
        .split("export interface CgPostFilters {")
        .nth(1)
        .and_then(|s| s.split("}\n").next())
        .expect("CgPostFilters block");
    assert!(
        !filters.contains(r#""id""#) && !filters.contains(r#""id__"#),
        "the PK must not be a filter key; got:\n{filters}",
    );
}

/// `.from(...)` keys off the exposed tables, and the client + envelope exist.
#[test]
fn resource_map_and_client_are_present() {
    let c = client();
    assert_has(
        &c,
        r#""cg_post": { row: CgPost; filters: CgPostFilters; ordering: CgPostOrdering; create: CgPostCreate; update: CgPostUpdate };"#,
    );
    assert_has(&c, "export class Umbral {");
    assert_has(&c, "from<K extends keyof UmbralResources>");
    assert_has(&c, "export interface Paginated<T> {");
    // Default paginator is None → envelope is just results + count.
    assert_has(&c, "  results: T[];");
    assert_has(&c, "  count: number;");
}

/// Ordering type covers every visible column, both directions.
#[test]
fn ordering_type_covers_columns_both_directions() {
    let c = client();
    assert_has(&c, "export type CgPostOrdering =");
    assert_has(&c, r#""title""#);
    assert_has(&c, r#""-title""#);
}

/// Extract the body of one interface (between its `{` and the closing `}\n`).
fn block<'a>(client: &'a str, decl: &str) -> &'a str {
    client
        .split(decl)
        .nth(1)
        .and_then(|s| s.split("}\n").next())
        .unwrap_or_else(|| panic!("no `{decl}` block in:\n{client}"))
}

/// The Create DTO includes creatable fields — including `noedit` (you can set a
/// slug on create) — with required vs optional per nullability/defaults, and
/// omits every server-managed column.
#[test]
fn create_dto_includes_noedit_and_omits_server_managed() {
    let c = client();
    let create = block(&c, "export interface CgPostCreate {");

    // required (non-null, no default) — no `?`.
    assert!(create.contains("title: string;"), "got:\n{create}");
    assert!(
        create.contains("author: string;"),
        "FK required; got:\n{create}"
    );
    // noedit IS creatable — the "set a username on create" case.
    assert!(
        create.contains("slug: string;"),
        "noedit must be creatable; got:\n{create}"
    );
    // nullable → optional.
    assert!(create.contains("body?: string | null;"), "got:\n{create}");

    // server-managed / stripped columns are absent.
    for gone in ["id", "internal", "created_at"] {
        assert!(
            !create.contains(&format!("{gone}:")) && !create.contains(&format!("{gone}?:")),
            "`{gone}` must be omitted from Create; got:\n{create}",
        );
    }
}

/// The Update DTO is a partial (every field optional) and — the headline — drops
/// `noedit`: a slug set on create cannot be changed.
#[test]
fn update_dto_is_partial_and_drops_noedit() {
    let c = client();
    let update = block(&c, "export interface CgPostUpdate {");

    assert!(
        update.contains("title?: string;"),
        "partial; got:\n{update}"
    );
    assert!(update.contains("author?: string;"), "got:\n{update}");
    assert!(
        !update.contains("slug"),
        "a noedit column must not be updatable; got:\n{update}",
    );
    // Same server-managed exclusions as Create.
    for gone in ["id", "internal", "created_at"] {
        assert!(
            !update.contains(gone),
            "`{gone}` must be omitted from Update; got:\n{update}"
        );
    }
}

/// `.create` / `.update` / `.delete` exist on the client and the resource map
/// carries the write types.
#[test]
fn client_exposes_write_operations() {
    let c = client();
    assert_has(&c, "create: CgPostCreate; update: CgPostUpdate");
    assert_has(&c, "create<K extends keyof UmbralResources>");
    assert_has(&c, "update<K extends keyof UmbralResources>");
    assert_has(&c, "delete<K extends keyof UmbralResources>");
}

/// The realtime subscription surface — a typed `.on(table, {created,updated,
/// deleted}, {group})` over the SSE stream the realtime plugin already serves.
#[test]
fn client_exposes_typed_realtime_on() {
    let c = client();
    assert_has(&c, "export interface Subscription {");
    assert_has(&c, "export interface ModelEvents<Row> {");
    assert_has(&c, "on<K extends keyof UmbralResources>");
    assert_has(&c, "ModelEvents<Partial<UmbralResources[K][\"row\"]>>");
    // Subscribes to the realtime plugin's SSE stream.
    assert_has(&c, "new EventSource(");
    assert_has(&c, "/sse?groups=");
    assert_has(&c, "realtimePath?: string;");
}
