//! gaps3 #38 / Kikosi #1 — the typed client (`client.js` + `client.d.ts`).
//!
//! Drives `client_gen::generate_for` with real `#[derive(Model)]` types (so the
//! derive → registry → generator path is under test) and asserts the structural
//! guarantees a frontend relies on: a `Filters` type per model whose keys are
//! exactly the REST-filterable (column, lookup) pairs, whose value types match
//! the field types, with the primary key excluded — plus the write DTOs, the
//! per-model id types, and the `Umbral` runtime that keys `.from(...)` off the
//! exposed tables.
//!
//! Types live in `client.d.ts`; the runtime (one copy, no transpile step) lives
//! in `client.js`. Each assertion targets whichever file the guarantee belongs
//! to.
//!
//! The generator reads `umbral_rest` config via `OnceLock`s that default
//! gracefully when unset (exposed, filters on, base path `/api`, no pagination),
//! so no `App::build` is needed here.

use serde::{Deserialize, Serialize};
use umbral::migrate::ModelMeta;
use umbral_openapi::client_gen::GeneratedClient;

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

/// Uuid primary key — its `.get`/`.update`/`.delete` id must type as `string`,
/// distinct from the i64-PK `CgPost` (`number`) and the String-slug-PK
/// `CgAuthor` (`string`). Proves the id type is per-model, not a global union.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "cg_ticket")]
pub struct CgTicket {
    pub id: uuid::Uuid,
    pub subject: String,
}

fn generated() -> GeneratedClient {
    umbral_openapi::client_gen::generate_for(&[
        ModelMeta::for_::<CgAuthor>(),
        ModelMeta::for_::<CgPost>(),
        ModelMeta::for_::<CgTicket>(),
    ])
}

/// The declaration file — every type.
fn dts() -> String {
    generated().dts
}

/// The runtime module — one copy, no types.
fn js() -> String {
    generated().js
}

#[track_caller]
fn assert_has(haystack: &str, needle: &str) {
    assert!(
        haystack.contains(needle),
        "expected to find:\n  {needle}\nin:\n{haystack}",
    );
}

/// The declaration file carries the row interfaces and choice unions.
#[test]
fn dts_has_the_row_types() {
    let d = dts();
    assert_has(&d, "export interface CgPost {");
    assert_has(
        &d,
        r#"export type CgPostStatus = "draft" | "published" | "archived";"#,
    );
    assert_has(&d, "export interface CgAuthor {");
}

/// The headline feature: a per-model filter type whose keys are the exact
/// query-params the REST list endpoint accepts, typed to the field's type.
#[test]
fn filters_type_lists_every_lookup_typed_to_the_field() {
    let d = dts();
    assert_has(&d, "export interface CgPostFilters {");

    // choices column → the enum union, plus __ne and __in (array of it).
    assert_has(&d, r#"  "status"?: CgPostStatus;"#);
    assert_has(&d, r#"  "status__in"?: CgPostStatus[];"#);

    // numeric column → number, with the comparison lookups.
    assert_has(&d, r#"  "views__gte"?: number;"#);
    assert_has(&d, r#"  "views__lt"?: number;"#);

    // text column → substring lookups are string.
    assert_has(&d, r#"  "title__contains"?: string;"#);

    // nullable column → __isnull is boolean.
    assert_has(&d, r#"  "body__isnull"?: boolean;"#);
}

/// The reason to generate rather than hand-write: an FK filter value is the
/// target's PK type. `author` points at a String-PK model, so it's `string`.
#[test]
fn foreign_key_filter_value_is_the_targets_pk_type() {
    let d = dts();
    assert_has(&d, r#"  "author"?: string;"#);
    assert!(
        !d.contains(r#"  "author"?: number;"#),
        "an FK to a String-PK model must filter by string, not number:\n{d}",
    );
}

/// The primary key is not a filter parameter (matches the REST contract, which
/// excludes the PK from filter params).
#[test]
fn primary_key_is_not_filterable() {
    let d = dts();
    let filters = d
        .split("export interface CgPostFilters {")
        .nth(1)
        .and_then(|s| s.split("}\n").next())
        .expect("CgPostFilters block");
    assert!(
        !filters.contains(r#""id""#) && !filters.contains(r#""id__"#),
        "the PK must not be a filter key; got:\n{filters}",
    );
}

/// `.from(...)` keys off the exposed tables; the runtime exports the classes the
/// declarations describe.
#[test]
fn resource_map_and_client_are_present() {
    let d = dts();
    assert_has(
        &d,
        r#""cg_post": { row: CgPost; filters: CgPostFilters; ordering: CgPostOrdering; create: CgPostCreate; update: CgPostUpdate; id: number };"#,
    );
    assert_has(&d, "export declare class Umbral {");
    assert_has(&d, "from<K extends keyof UmbralResources>");
    assert_has(&d, "export interface Paginated<T> {");
    // Default paginator is None → envelope is just results + count.
    assert_has(&d, "  results: T[];");
    assert_has(&d, "  count: number;");

    let j = js();
    assert_has(&j, "export class Umbral {");
    assert_has(&j, "export class Query {");
    assert_has(&j, "export class UmbralError extends Error {");
}

/// Ordering type covers every visible column, both directions.
#[test]
fn ordering_type_covers_columns_both_directions() {
    let d = dts();
    assert_has(&d, "export type CgPostOrdering =");
    assert_has(&d, r#""title""#);
    assert_has(&d, r#""-title""#);
}

/// Extract the body of one interface (between its `{` and the closing `}\n`).
fn block<'a>(dts: &'a str, decl: &str) -> &'a str {
    dts.split(decl)
        .nth(1)
        .and_then(|s| s.split("}\n").next())
        .unwrap_or_else(|| panic!("no `{decl}` block in:\n{dts}"))
}

/// The Create DTO includes creatable fields — including `noedit` (you can set a
/// slug on create) — with required vs optional per nullability/defaults, and
/// omits every server-managed column.
#[test]
fn create_dto_includes_noedit_and_omits_server_managed() {
    let d = dts();
    let create = block(&d, "export interface CgPostCreate {");

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
    let d = dts();
    let update = block(&d, "export interface CgPostUpdate {");

    assert!(
        update.contains("title?: string;"),
        "partial; got:\n{update}"
    );
    assert!(update.contains("author?: string;"), "got:\n{update}");
    assert!(
        !update.contains("slug"),
        "a noedit column must not be updatable; got:\n{update}",
    );
    for gone in ["id", "internal", "created_at"] {
        assert!(
            !update.contains(gone),
            "`{gone}` must be omitted from Update; got:\n{update}"
        );
    }
}

/// `.create` / `.update` / `.delete` are declared AND implemented.
#[test]
fn client_exposes_write_operations() {
    let d = dts();
    assert_has(&d, "create: CgPostCreate; update: CgPostUpdate");
    assert_has(&d, "create<K extends keyof UmbralResources>");
    assert_has(&d, "update<K extends keyof UmbralResources>");
    assert_has(&d, "delete<K extends keyof UmbralResources>");

    let j = js();
    assert_has(&j, "create(table, data)");
    assert_has(&j, "update(table, id, data)");
    assert_has(&j, "async delete(table, id)");
}

/// Each resource carries its OWN primary-key type, not a global union: the i64
/// PK is `number`, the Uuid and String-slug PKs are `string`. This is what makes
/// `.get`/`.update`/`.delete` reject the wrong id type per model.
#[test]
fn each_resource_has_its_own_id_type() {
    let d = dts();
    assert!(
        d.lines()
            .any(|l| l.contains("\"cg_post\":") && l.contains("id: number }")),
        "an i64 PK must type as number; got:\n{d}",
    );
    assert!(
        d.lines()
            .any(|l| l.contains("\"cg_ticket\":") && l.contains("id: string }")),
        "a Uuid PK must type as string; got:\n{d}",
    );
    assert!(
        d.lines()
            .any(|l| l.contains("\"cg_author\":") && l.contains("id: string }")),
        "a String PK must type as string; got:\n{d}",
    );
    // The old global union is gone.
    assert!(
        !d.contains("UmbralId"),
        "the global id union must be gone; got:\n{d}"
    );
    assert_has(&d, "id: UmbralResources[K][\"id\"]");
}

/// The realtime subscription surface is typed in the `.d.ts`.
#[test]
fn realtime_on_is_typed() {
    let d = dts();
    assert_has(&d, "export interface Subscription {");
    assert_has(&d, "export interface ModelEvents<Row> {");
    assert_has(&d, "on<K extends keyof UmbralResources>");
    assert_has(&d, "ModelEvents<Partial<UmbralResources[K][\"row\"]>>");
}

/// **The anti-duplication guarantee.** `.on(...)` must DELEGATE to the realtime
/// plugin's already-served runtime (`umbral.realtime.model`), which shares ONE
/// SSE connection across every tab via SharedWorker and brings presence +
/// graceful degradation. It must NOT hand-roll its own `EventSource`: that opens
/// one connection per subscription and exhausts the browser's ~6-per-origin cap
/// at six models, while re-implementing (worse) logic that already exists.
#[test]
fn realtime_delegates_and_never_opens_its_own_eventsource() {
    let j = js();
    assert_has(&j, "/client.js`");
    assert_has(&j, "rt.model(String(table)");
    assert_has(&j, "g.umbral && g.umbral.realtime");
    // It never CONSTRUCTS a transport of its own (the word appears in the
    // explanatory comment; what must not exist is the construction).
    assert!(
        !j.contains("new EventSource") && !j.contains("new WebSocket"),
        "the generated client must delegate to umbral.realtime, not open its own \
         connection (one per subscription exhausts the browser's per-origin cap); \
         got:\n{j}",
    );
}

/// The runtime is a self-contained ES module: no imports (the types erase, so
/// there is nothing to import) and no leftover type annotations. That's what
/// makes it loadable from a plain `<script type="module">` with no build step.
#[test]
fn js_is_a_self_contained_es_module() {
    let j = js();
    assert!(
        !j.contains("import ") && !j.contains("from \"./"),
        "client.js must be self-contained — no imports; got:\n{j}",
    );
    assert_has(&j, "export class Umbral {");
    assert!(
        !j.contains("): Promise<") && !j.contains("?: string;"),
        "client.js must be plain JS — no TypeScript annotations leaked in; got:\n{j}",
    );
}
