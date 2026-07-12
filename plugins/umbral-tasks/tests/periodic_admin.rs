//! The schedules are visible in the admin (gaps3 #49).
//!
//! `admin_model()` shows the QUEUE — what ran, what failed, what's pending. It
//! says nothing about what is *scheduled*, so an operator could watch a task fire
//! hourly with no way to see its cron expression, its next run, or turn it off.
//! Pausing a runaway schedule at 3am required a `psql` session.
//!
//! `AdminModel`'s fields are private outside umbral-admin, so — as the sibling
//! `admin.rs` test does — these assert through `Debug`.

#[cfg(feature = "admin")]
fn dbg_model() -> String {
    format!("{:?}", umbral_tasks::periodic_admin_model())
}

/// Every column the admin renders must exist on `periodic_task`. A name that
/// doesn't 500s the page at request time; catching it here is the whole point.
#[cfg(feature = "admin")]
#[test]
fn every_listed_column_exists_on_the_model() {
    let dbg = dbg_model();
    let meta = umbral::migrate::ModelMeta::for_::<umbral_tasks::PeriodicTask>();
    let cols: Vec<&str> = meta.fields.iter().map(|c| c.name.as_str()).collect();

    assert!(
        dbg.contains("periodic_task"),
        "targets periodic_task: {dbg}"
    );
    for col in [
        "name", "task", "schedule", "next_run", "last_run", "enabled", "payload",
    ] {
        assert!(
            cols.contains(&col),
            "the admin model names `{col}`, which is not a column on periodic_task ({cols:?})",
        );
        assert!(
            dbg.contains(col),
            "`{col}` should be surfaced; debug = {dbg}"
        );
    }
}

/// The operator action that matters: pause a runaway schedule without psql.
#[cfg(feature = "admin")]
#[test]
fn it_offers_an_enable_disable_action() {
    let dbg = dbg_model();
    assert!(
        dbg.contains("toggle_enabled"),
        "an operator must be able to pause a schedule from the admin; debug = {dbg}",
    );
}
