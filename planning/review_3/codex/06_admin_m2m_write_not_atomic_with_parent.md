# Admin Many-To-Many Writes Are Not Atomic With Parent Save

Category: Correctness
Severity: Medium

## Finding

Admin create/update saves the parent and inline rows in a transaction, then applies many-to-many selections after that transaction has committed. If the many-to-many write fails, the response can be an error while the parent record has already been created or updated.

## Evidence

- `plugins/umbral-admin/src/handlers/crud.rs:454-493` saves parent plus inlines inside a transaction.
- `plugins/umbral-admin/src/handlers/crud.rs:352-364` applies many-to-many selections after create.
- `plugins/umbral-admin/src/handlers/crud.rs:687-703` applies many-to-many selections after update.
- The source comment notes the follow-up need to fold many-to-many handling into the parent transaction.

## Risk

Admin users can observe partial writes: parent fields persist but relationship changes fail. This can create inconsistent admin state and hard-to-reproduce data repair cases.

## Recommendation

Move many-to-many writes into the same transaction as the parent and inline writes. If the relationship write fails, the whole admin save should roll back.

## Suggested Tests

- Force a join table insert failure during admin create and assert the parent row is not created.
- Force a join table update failure during admin update and assert parent fields are unchanged.

