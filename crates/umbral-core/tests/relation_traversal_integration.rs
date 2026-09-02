//! Phase 1, Task 7 — the full behavioral suite for relation traversal.
//!
//! Every assertion goes through a GENERATED accessor (`<Model>Relations`
//! trait method, or the derive's separate reverse-FK / reverse-O2O trait
//! methods) — never a hand-written `to_one_hop` / `to_many_hop` call. This
//! file proves the whole Phase-1 feature end to end: forward FK/O2O, reverse
//! O2O, M2M, reverse-FK, a genuine 5-hop mixed chain, leaf dedupe, and a
//! reverse-FK-as-leaf DEEP chain — all against the AMBIENT pool (no `.on(&pool)`
//! anywhere in this file; see the harness note below).
//!
//! Behavioral, per the project's testing rule: real rows through the actual
//! public accessors, reading the object graph back — never a SQL-string
//! assertion as a proxy (the one `to_sql()` probe in this file runs ALONGSIDE
//! a round-trip, never instead of one).
//!
//! Schema (all-to-one prefix capable of 3 hops, then a 2-hop to-many tail):
//!
//! ```text
//! User    --reverse-O2O-->  Employee    (Employee's unique FK is the anchor)
//! Employee --FK-->          Department  (required)
//! Department --FK-->        Company     (required)
//! Company  --M2M-->         Project
//! Project  --M2M-->         Tag
//! ```
//!
//! `user.employee().department().company().projects().tags()` is therefore a
//! genuine 5-hop SUPPORTED chain (reverse-O2O, Fk, Fk, M2M, M2M — an
//! all-to-one prefix of 3 THEN a to-many tail of 2), and
//! `user.employee().department().employee_set()` is a 3-hop chain ending in a
//! reverse-FK LEAF — the exact shape Task 4 implemented in the resolver but
//! left behaviorally untested, and which Task 5's reverse-FK accessor did NOT
//! wire into the chain machinery until this task closed that gap (see the
//! Task 7 report for the macro fix).

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbral::orm::{Aggregate, ForeignKey, M2M, OneToOne};
use umbral_core::db;

// =========================================================================
// Models
// =========================================================================

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "rti_tag")]
pub struct Tag {
    #[umbral(primary_key)]
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "rti_project")]
pub struct Project {
    #[umbral(primary_key)]
    pub id: i64,
    pub name: String,
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbral(m2m = "rti_tag")]
    pub tags: M2M<Tag>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "rti_company")]
pub struct Company {
    #[umbral(primary_key)]
    pub id: i64,
    pub name: String,
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbral(m2m = "rti_project")]
    pub projects: M2M<Project>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "rti_department")]
pub struct Department {
    #[umbral(primary_key)]
    pub id: i64,
    pub name: String,
    /// Required forward FK — a to-one prefix hop. Also drives the reverse-FK
    /// leaf accessor `department.employee_set()` (item 8) on `Department`.
    pub company: ForeignKey<Company>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "rti_employee")]
pub struct Employee {
    #[umbral(primary_key)]
    pub id: i64,
    pub name: String,
    /// Required forward FK — a to-one prefix hop, AND the anchor for
    /// `department.employee_set()` (the reverse-FK leaf, item 8).
    pub department: ForeignKey<Department>,
    /// Child-side of the OneToOne back to `User` (a UNIQUE FK). Generates the
    /// single `user.employee() -> Relation<Employee>` accessor (reverse-O2O,
    /// unified per Task 5) and `employee.user() -> Relation<User>` (O2O
    /// child-side forward accessor, item 2).
    #[umbral(unique)]
    pub user: ForeignKey<User>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "rti_user")]
pub struct User {
    #[umbral(primary_key)]
    pub id: i64,
    pub name: String,
    /// Parent-side reverse-O2O back-link, carries no DB column — declaring it
    /// is documentation only (see `relation_codegen.rs`); the real
    /// `user.employee()` accessor comes from `Employee::user`'s unique FK.
    #[sqlx(skip)]
    #[serde(skip)]
    pub employee: OneToOne<Employee>,
}

/// A second, unrelated model purely to exercise "forward FK required, target
/// row missing" (item 1) without disturbing the main graph above: a Note that
/// points at a Department id that has never been (or is no longer) inserted.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "rti_note")]
pub struct Note {
    #[umbral(primary_key)]
    pub id: i64,
    pub body: String,
    /// Required FK — deliberately left dangling for one seeded row.
    pub department: ForeignKey<Department>,
    /// Nullable FK, for the nullable-FK Option round trip (item 1).
    pub reviewer_dept: Option<ForeignKey<Department>>,
}

// =========================================================================
// Harness — one boot per process, ambient pool for every test in this file.
//
// `App::builder().database("default", pool.clone()).build()` publishes the
// pool into the crate's ambient `OnceLock` (see `umbral_core::db`), and every
// call below — `X::objects()`, every generated relation accessor, every
// terminal — resolves that pool implicitly. NOT ONE test in this file calls
// `.on(&pool)`: pool-free chaining is the default here, proving item 9 for
// the whole suite (not just one token test), the same pattern already
// established by `relation_codegen.rs`.
// =========================================================================

static BOOT: OnceCell<sqlx::SqlitePool> = OnceCell::const_new();

async fn boot() -> sqlx::SqlitePool {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let pool = db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Tag>()
            .model::<Project>()
            .model::<Company>()
            .model::<Department>()
            .model::<Employee>()
            .model::<User>()
            .model::<Note>()
            .build()
            .expect("App::build");

        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        // --- Users: ada=1 (has an employee), grace=2 (no employee row) ---
        for name in &["ada", "grace"] {
            sqlx::query("INSERT INTO rti_user (name) VALUES (?)")
                .bind(*name)
                .execute(&pool)
                .await
                .expect("seed user");
        }

        // --- Companies: acme=1 ---
        sqlx::query("INSERT INTO rti_company (name) VALUES ('acme')")
            .execute(&pool)
            .await
            .expect("seed company");

        // --- Departments: eng=1 (-> acme), sales=2 (-> acme, no employees) ---
        for (name, company) in &[("eng", 1_i64), ("sales", 1_i64)] {
            sqlx::query("INSERT INTO rti_department (name, company) VALUES (?, ?)")
                .bind(*name)
                .bind(*company)
                .execute(&pool)
                .await
                .expect("seed department");
        }

        // --- Employees: ada-emp=1 (-> eng, user=ada), bob-emp=2 (-> eng, no user) ---
        sqlx::query("INSERT INTO rti_employee (name, department, user) VALUES ('ada-emp', 1, 1)")
            .execute(&pool)
            .await
            .expect("seed employee ada");
        // bob has no `user` link — but `user` is a required unique FK on this
        // model, so give bob his OWN dummy user row (id will not be referenced
        // by anything else) purely to satisfy the NOT NULL constraint; bob is
        // never reached via `<user>.employee()` in these tests, only via
        // `department.employee_set()`.
        sqlx::query("INSERT INTO rti_user (name) VALUES ('bob-shadow')")
            .execute(&pool)
            .await
            .expect("seed bob's shadow user");
        sqlx::query("INSERT INTO rti_employee (name, department, user) VALUES ('bob-emp', 1, 3)")
            .execute(&pool)
            .await
            .expect("seed employee bob");

        // --- Projects: web=1 (-> acme), infra=2 (-> acme) ---
        for name in &["web", "infra"] {
            sqlx::query("INSERT INTO rti_project (name) VALUES (?)")
                .bind(*name)
                .execute(&pool)
                .await
                .expect("seed project");
        }
        sqlx::query("INSERT INTO rti_company_projects (parent_id, child_id) VALUES (1, 1), (1, 2)")
            .execute(&pool)
            .await
            .expect("seed acme->projects");

        // --- Tags: rust=1, backend=2, frontend=3 ---
        // web(1)   -> rust(1), frontend(3)
        // infra(2) -> rust(1), backend(2)
        // rust is reachable via BOTH projects — the dedupe fixture.
        for name in &["rust", "backend", "frontend"] {
            sqlx::query("INSERT INTO rti_tag (name) VALUES (?)")
                .bind(*name)
                .execute(&pool)
                .await
                .expect("seed tag");
        }
        for (parent, child) in &[
            (1_i64, 1_i64),
            (1_i64, 3_i64),
            (2_i64, 1_i64),
            (2_i64, 2_i64),
        ] {
            sqlx::query("INSERT INTO rti_project_tags (parent_id, child_id) VALUES (?, ?)")
                .bind(*parent)
                .bind(*child)
                .execute(&pool)
                .await
                .expect("seed project->tags");
        }

        // --- Note: a required-FK-missing-target fixture (item 1) ---
        // note 1 -> department 1 (fine); note 2 -> department 999 (dangling).
        // reviewer_dept is nullable: note 1 has one set, note 2 does not.
        sqlx::query("INSERT INTO rti_note (body, department, reviewer_dept) VALUES ('n1', 1, 2)")
            .execute(&pool)
            .await
            .expect("seed note 1");
        // `department = 999` deliberately references a row that doesn't
        // exist — the migration engine emits a real FK constraint for
        // `ForeignKey<Department>`, so inserting the dangling row needs FK
        // enforcement off for this one connection (put back on immediately
        // after, on the SAME connection, so no other insert in this pool is
        // ever affected).
        {
            let mut conn = pool
                .acquire()
                .await
                .expect("acquire a connection for the dangling-FK insert");
            sqlx::query("PRAGMA foreign_keys = OFF")
                .execute(&mut *conn)
                .await
                .expect("disable FK enforcement");
            sqlx::query(
                "INSERT INTO rti_note (body, department, reviewer_dept) VALUES ('n2', 999, NULL)",
            )
            .execute(&mut *conn)
            .await
            .expect("seed note 2 (dangling required FK)");
            sqlx::query("PRAGMA foreign_keys = ON")
                .execute(&mut *conn)
                .await
                .expect("re-enable FK enforcement");
        }

        pool
    })
    .await
    .clone()
}

async fn fetch_user(name: &str) -> User {
    User::objects()
        .filter(user::NAME.eq(name))
        .get()
        .await
        .expect("fetch user")
}

async fn fetch_department(name: &str) -> Department {
    Department::objects()
        .filter(department::NAME.eq(name))
        .get()
        .await
        .expect("fetch department")
}

async fn fetch_company(name: &str) -> Company {
    Company::objects()
        .filter(company::NAME.eq(name))
        .get()
        .await
        .expect("fetch company")
}

async fn fetch_note(body: &str) -> Note {
    Note::objects()
        .filter(note::BODY.eq(body))
        .get()
        .await
        .expect("fetch note")
}

// =========================================================================
// 1. Forward FK -> object; nullable forward FK -> Option; missing REQUIRED
//    FK target -> Err (never a silent None).
// =========================================================================

#[tokio::test]
async fn forward_fk_required_resolves_to_object() {
    boot().await;
    let note = fetch_note("n1").await;
    let dept = note.department().await.expect("required FK resolves");
    assert_eq!(dept.name, "eng");
}

#[tokio::test]
async fn forward_fk_nullable_returns_option_both_ways() {
    boot().await;

    let with = fetch_note("n1").await;
    let some = with
        .reviewer_dept()
        .get_opt()
        .await
        .expect("reviewer_dept query");
    assert_eq!(some.map(|d| d.name), Some("sales".to_string()));

    let without = fetch_note("n2").await;
    let none = without
        .reviewer_dept()
        .get_opt()
        .await
        .expect("no-reviewer-dept query");
    assert!(none.is_none(), "note n2 has a NULL reviewer_dept");
}

#[tokio::test]
async fn forward_fk_required_missing_target_is_err_not_none() {
    boot().await;
    let dangling = fetch_note("n2").await;

    // `.await` (== `.get()`) on a REQUIRED relation errors loudly when the
    // target row is absent — referential integrity is already broken, and
    // the spec forbids surfacing that as a silent `None`.
    let err = dangling
        .department()
        .await
        .expect_err("a dangling required FK must error, not resolve to nothing");
    assert!(
        matches!(err, sqlx::Error::RowNotFound),
        "expected RowNotFound, got: {err:?}"
    );
}

// =========================================================================
// 2. Forward O2O child-side -> object.
// =========================================================================

#[tokio::test]
async fn forward_o2o_child_side_resolves_parent() {
    boot().await;
    let ada_emp = Employee::objects()
        .filter(employee::NAME.eq("ada-emp"))
        .get()
        .await
        .expect("fetch ada-emp");
    let user = ada_emp.user().await.expect("employee.user() resolves");
    assert_eq!(user.name, "ada");
}

// =========================================================================
// 3. Reverse O2O parent-side -> `Option` both ways (closes Task 1's minor).
// =========================================================================

#[tokio::test]
async fn reverse_o2o_parent_side_option_round_trip() {
    boot().await;

    let ada = fetch_user("ada").await;
    let some = ada.employee().get_opt().await.expect("ada.employee()");
    assert_eq!(some.map(|e| e.name), Some("ada-emp".to_string()));

    // Awaiting directly (the REQUIRED shape) also resolves for ada.
    let emp = ada.employee().await.expect("ada.employee() awaits");
    assert_eq!(emp.name, "ada-emp");

    let grace = fetch_user("grace").await;
    let none = grace.employee().get_opt().await.expect("grace.employee()");
    assert!(none.is_none(), "grace has no employee row");
}

// =========================================================================
// 4. M2M forward -> QuerySet: `.fetch()` AND a chained `.filter().count()`.
// =========================================================================

#[tokio::test]
async fn m2m_forward_fetch_and_chained_filter_count() {
    boot().await;
    let acme = fetch_company("acme").await;

    let projects = acme
        .projects()
        .order_by(project::NAME.asc())
        .fetch()
        .await
        .expect("acme.projects().fetch()");
    let names: Vec<&str> = projects.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["infra", "web"]);

    let just_web = acme
        .projects()
        .filter(project::NAME.eq("web"))
        .count()
        .await
        .expect("chained filter().count()");
    assert_eq!(just_web, 1);
}

// =========================================================================
// 5. Reverse-FK `<child>_set()` -> `.fetch()` + `.aggregate()`.
// =========================================================================

#[tokio::test]
async fn reverse_fk_set_fetch_and_aggregate() {
    boot().await;
    let eng = fetch_department("eng").await;

    let employees = eng
        .employee_set()
        .order_by(employee::NAME.asc())
        .fetch()
        .await
        .expect("eng.employee_set().fetch()");
    let names: Vec<&str> = employees.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["ada-emp", "bob-emp"]);

    let summary = eng
        .employee_set()
        .aggregate(&[("n", Aggregate::count())])
        .await
        .expect("eng.employee_set().aggregate()");
    assert_eq!(summary["n"], serde_json::json!(2));

    // sales has an FK from department -> company but zero employees.
    let sales = fetch_department("sales").await;
    let empty_count = sales
        .employee_set()
        .count()
        .await
        .expect("sales.employee_set().count()");
    assert_eq!(empty_count, 0);
}

// =========================================================================
// 6. The headline: a genuine 5-hop mixed chain of a SUPPORTED shape —
//    reverse-O2O -> Fk -> Fk -> M2M -> M2M — entirely through generated
//    accessors, resolving to the deduped leaf set in ONE flat statement
//    (plus the ONE subquery the all-to-one prefix folds into). The `to_sql()`
//    probe runs ALONGSIDE the round-trip, never instead of it.
// =========================================================================

#[tokio::test]
async fn five_hop_mixed_chain_resolves_to_deduped_leaf() {
    boot().await;
    let ada = fetch_user("ada").await;

    let chain = ada
        .employee() // reverse-O2O  (to-one)  -> Employee
        .department() // Fk           (to-one)  -> Department
        .company() // Fk           (to-one)  -> Company
        .projects() // M2M          (to-many) -> QuerySet<Project>
        .tags(); // M2M          (to-many) -> QuerySet<Tag>

    // Statement-count probe: the whole 5-hop traversal renders as ONE outer
    // SELECT (the leaf `tags` query, with the 2-hop M2M tail as 2 junction
    // JOINs) plus exactly ONE nested subquery (the 3-hop all-to-one prefix —
    // reverse-O2O + Fk + Fk — folded into a single `IN (SELECT …)`, itself 3
    // JOINs chaining user->employee->department->company) — TWO SELECTs
    // total (never one round trip per hop, regardless of how deep the prefix
    // or the to-many tail goes), and exactly 5 JOINs (3 prefix + 2 tail).
    let sql = chain.to_sql();
    let select_count = sql.matches("SELECT").count();
    assert_eq!(
        select_count, 2,
        "expected exactly 2 SELECTs (1 outer leaf + 1 prefix-pivot subquery), got: {sql}"
    );
    assert_eq!(
        sql.matches(" JOIN ").count(),
        5,
        "expected exactly 5 JOINs (3 for the to-one prefix, 2 for the M2M tail): {sql}"
    );

    let tags = chain
        .order_by(tag::NAME.asc())
        .fetch()
        .await
        .expect("5-hop chain resolves");
    let names: Vec<&str> = tags.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["backend", "frontend", "rust"],
        "rust is reachable via BOTH web and infra but must appear ONCE by default"
    );
}

// =========================================================================
// 7. Dedupe: a to-many -> to-many chain where a leaf is reachable via
//    multiple parents. Default fetch dedupes; `.with_duplicates()` doesn't.
//    Reuses the same acme -> {web, infra} -> tags graph as a 2-hop subchain.
// =========================================================================

#[tokio::test]
async fn to_many_chain_dedupes_by_default_and_with_duplicates_opts_out() {
    boot().await;
    let acme = fetch_company("acme").await;

    let deduped = acme
        .projects()
        .tags()
        .order_by(tag::NAME.asc())
        .fetch()
        .await
        .expect("default fetch dedupes");
    let names: Vec<&str> = deduped.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, vec!["backend", "frontend", "rust"]);

    let with_dupes = acme
        .projects()
        .tags()
        .with_duplicates()
        .order_by(tag::NAME.asc())
        .fetch()
        .await
        .expect("with_duplicates surfaces join multiplicity");
    let names: Vec<&str> = with_dupes.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["backend", "frontend", "rust", "rust"],
        "rust reachable via both web and infra must appear TWICE under with_duplicates"
    );

    // count() honours the same distinct-by-default / multiplicity opt-out.
    let distinct_count = acme
        .projects()
        .tags()
        .count()
        .await
        .expect("distinct count");
    assert_eq!(distinct_count, 3);
    let raw_count = acme
        .projects()
        .tags()
        .with_duplicates()
        .count()
        .await
        .expect("raw count");
    assert_eq!(raw_count, 4);
}

// =========================================================================
// 8. Reverse-FK-as-leaf DEEP chain (Task 4's deferred minor, closed by this
//    task's macro fix — see the Task 7 report): a to-one PREFIX (reverse-O2O
//    then Fk) ending in a reverse-FK LEAF resolves through the GENERATED
//    `department.employee_set()` accessor, called off a `Relation<Department>`
//    handle rather than a bare loaded object.
// =========================================================================

#[tokio::test]
async fn reverse_fk_leaf_resolves_after_a_to_one_prefix() {
    boot().await;
    let ada = fetch_user("ada").await;

    // `.department()` returns `Relation<Department>` (a handle, not a loaded
    // Department) — calling `.employee_set()` on THAT is the proof this is a
    // real deep chain, not an object-rooted call.
    let colleagues = ada
        .employee() // reverse-O2O -> Employee
        .department() // Fk          -> Department (still a Relation<Department>)
        .employee_set() // ReverseFk (leaf) -> QuerySet<Employee>
        .order_by(employee::NAME.asc())
        .fetch()
        .await
        .expect("reverse-FK leaf resolves after a to-one prefix");
    let names: Vec<&str> = colleagues.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["ada-emp", "bob-emp"],
        "eng department's employee_set, reached via ada's own reverse-O2O + Fk prefix"
    );
}

// =========================================================================
// 9. Ambient pool: every test above already runs with NO `.on(&pool)`
//    anywhere in this file — `boot()`'s `App::builder()...build()` publishes
//    the pool into the ambient `OnceLock` once, and every accessor / terminal
//    resolves it implicitly. This test just makes that explicit with a fresh
//    single-hop assertion, so the property has its own named witness too.
// =========================================================================

#[tokio::test]
async fn ambient_pool_resolves_with_no_explicit_on_call() {
    boot().await;
    let acme = fetch_company("acme").await; // no `.on(&pool)` on this query either
    let count = acme.projects().count().await.expect("ambient-pool count");
    assert_eq!(count, 2);
}
