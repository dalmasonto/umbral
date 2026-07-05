//! audit_2 plugin-storage-tasks #5 — `TaskRow` declares a composite index on
//! the claim query's selective predicate (`status`, `run_at`), so a worker poll
//! is an index range scan instead of a full `task_row` scan. Verifies the index
//! is declared on the model and renders as a real `CREATE INDEX` on both
//! backends (which the autodetector now emits as an `AddIndex` for existing DBs).

use umbral::migrate::{ModelMeta, Operation, render_operation_for};
use umbral::orm::Model as _;
use umbral_tasks::TaskRow;

#[test]
fn task_row_declares_the_claim_index() {
    assert!(
        TaskRow::INDEXES.iter().any(|g| *g == ["status", "run_at"]),
        "TaskRow must declare a composite (status, run_at) index; got {:?}",
        TaskRow::INDEXES
    );
}

#[test]
fn create_table_emits_the_claim_index_on_both_backends() {
    let meta = ModelMeta::for_::<TaskRow>();
    let op = Operation::CreateTable {
        table: TaskRow::TABLE.to_string(),
        columns: meta.fields.clone(),
        unique_together: meta.unique_together.clone(),
        indexes: meta.indexes.clone(),
    };
    for backend in ["postgres", "sqlite"] {
        let sql = render_operation_for(&op, backend).join("\n");
        assert!(
            sql.contains("CREATE INDEX")
                && sql.contains("\"idx_task_row_status_run_at\"")
                && sql.contains("\"status\"")
                && sql.contains("\"run_at\""),
            "[{backend}] CreateTable must emit the composite claim index; got {sql}"
        );
    }
}
