# FK-Save Coercion Fix — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:systematic-debugging for Phase A, then superpowers:executing-plans for the fix. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Make every dynamic write path bind a foreign-key id against its *target PK's* `SqlType` (bigint for an i64-PK parent), so saving an FK through the admin / JSON / form layer stops emitting `column "plugin" is of type bigint but expression is of type text` (gaps2 #42).

**Architecture:** Diagnosis-first. The bug is a path the existing `form_str_to_sea_value` unit test (`dynamic.rs:2606`) never exercises — its FK arm is correct, so the failure lives elsewhere in the live-meta write chain. Phase A writes a behavioral round-trip test that drives the *real* `DynQuerySet::for_meta(meta).insert_form(...)` path against a seeded SQLite parent and pins the exact bind type with a debug probe; Phase B fixes whichever of three enumerated suspects the probe confirms; Phase C regression-pins the dynamic path AND asserts the typed `create` path binds identically so the two can't diverge again. The guaranteed contract: *no umbral write path ever binds a numeric-PK FK as TEXT.*

**Tech Stack:** Rust, sqlx, sea-query

---

## What the investigation already establishes (read before starting)

These facts are confirmed by reading the code; they narrow the suspect set and tell you *where* the bug cannot be:

- The Model derive emits FK columns with `ty == SqlType::ForeignKey` and `fk_target == Some(<T as Model>::TABLE)` (`umbral-macros/src/lib.rs:2098`, `:1128`). `From<&FieldSpec> for Column` copies both straight through (`migrate.rs:838`, `:841`). So the live `ModelMeta` the admin uses **does** carry `ty == ForeignKey`. Suspect B (meta loses `ForeignKey`) is therefore *unlikely* but cheaply falsified by the Phase A probe — keep it in the tree until the probe prints the live `col.ty`.
- `form_str_to_sea_value`'s FK arm (`dynamic.rs:1954`) coerces a numeric string → `SeaValue::BigInt` for any non-Text/non-Uuid target — including when `fk_target_pk_sql_type` returns `None` (the `_ =>` default). The helper test at `dynamic.rs:2606` passes precisely because that default is BigInt. So a value reaching THIS arm is bound correctly.
- **The actual TEXT bind lives in `json_to_sea_value`'s `ForeignKey` arm (`write.rs:428-430`):** `JsonValue::String(s) => Ok(SeaValue::String(...))`. This function has **no access to `fk_target`** (its signature is `(SqlType, &JsonValue, bool, &str)`), so it cannot tell an i64-PK FK from a String-PK FK — it binds every string-valued FK as TEXT. This arm is reached by:
  - `DynQuerySet::insert_json` / `update_json` (the admin/REST JSON write path), `dynamic.rs:1138`, which calls `json_to_sea_value(col.ty, json, ...)` for FK columns — and a JSON body that delivers the FK id as a *string* (`{"plugin": "1"}`) lands `JsonValue::String` → TEXT.
  - The typed `create` path via `build_insert_one_for` (`queryset/write_helpers.rs:67`), IF `serde_json::to_value` of the `ForeignKey<T>` produces a JSON *string*. (Today it produces a number for i64 targets, which is the only reason the typed path doesn't already crash — a fragile invariant the Phase C parallel assertion locks down.)
- `fk_target_pk_sql_type` (`dynamic.rs:1699`) already does the right resolution (`fk_target` → `pk_meta_for_table(target)` → PK `SqlType`); it returns `None` before `App::build` runs or when the target isn't registered. `pk_meta_for_table` (`migrate.rs:121`) reads the `OnceLock` registry.

**Conclusion going in:** the prime suspect (A) is `json_to_sea_value`'s FK arm binding TEXT for an i64-PK target because it can't see `fk_target`. Phase A's probe confirms *which* live path the failing model takes and *what* `col.ty` / `fk_target` / resolved target-PK the FK column actually has, so Phase B applies the matching branch with certainty.

---

## File Structure

```
crates/umbral-core/tests/fk_save_coercion.rs   (NEW — Phase A reproduction + Phase C regression pin)
crates/umbral-core/src/orm/write.rs            (EDIT — Phase B fix, json_to_sea_value FK arm)
crates/umbral-core/src/orm/dynamic.rs          (EDIT — Phase B, only under Suspect B/C branches)
planning/gaps2.md                              (EDIT — close #42 to a one-line stub)
planning/archive/gaps2-done.md                 (EDIT — #42 full write-up, verbatim)
documentation/docs/v0.0.1/orm/...              (no new page; bugfix to an existing surface)
```

The whole fix is one logical change → one commit (`fix(orm): ...`). The test file is new; the fix touches `write.rs` (and only conditionally `dynamic.rs`).

---

## Phase A — Reproduce against the real write path, then pin the root cause

### Task 1 — Behavioral reproduction test (drives the live `insert_form` path)

**Files:**
- `crates/umbral-core/tests/fk_save_coercion.rs` (NEW)
- Copy the harness shape from `crates/umbral-core/tests/json_form_parse.rs:33-68` (in-memory SQLite pool + `CREATE TABLE` + `App::builder()...build()` + `ModelMeta::for_::<T>()`).

Steps:

- [ ] Create `fk_save_coercion.rs`. Declare TWO models so the FK has a real i64-PK parent in the registry:
  ```rust
  #[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
  #[umbral(table = "fkparent")]
  pub struct Parent { pub id: i64, pub name: String }

  #[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
  #[umbral(table = "fkchild")]
  pub struct Child {
      pub id: i64,
      pub parent: ForeignKey<Parent>,   // i64-PK FK — the #42 shape
      pub body: String,
  }
  ```
  Mirror `PluginComment.plugin: ForeignKey<Plugin>` (Plugin PK is `id: i64`) — the real failing consumer.
- [ ] In `fresh_pool()`, `CREATE TABLE fkparent (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)` and `CREATE TABLE fkchild (id INTEGER PRIMARY KEY AUTOINCREMENT, parent INTEGER NOT NULL REFERENCES fkparent(id), body TEXT NOT NULL)`. The `parent` column is declared **`INTEGER`** — this is what makes the TEXT bind fail at execute time, exactly as Postgres `bigint` rejects a text expression. (SQLite is loosely typed, so also assert the bound SeaValue variant directly — see Task 2 — because a permissive SQLite may *store* a text "1" without erroring; the probe is what makes the bug deterministic on both backends.)
- [ ] `boot()` registers BOTH models via `.model::<Parent>().model::<Child>()` so `pk_meta_for_table("fkparent")` resolves post-build.
- [ ] Write the headline round-trip test `fk_id_as_string_round_trips_and_links_parent`:
  ```rust
  // seed a real parent
  let parent = Parent::objects().create(Parent { id: 0, name: "Acme".into() }).await.expect("seed parent");

  // submit the child form with the FK id AS A STRING (the form/admin shape)
  let mut form = HashMap::new();
  form.insert("parent".to_string(), parent.id.to_string());  // "1", a string
  form.insert("body".to_string(), "hello".to_string());

  let child_id = DynQuerySet::for_meta(&ModelMeta::for_::<Child>())
      .insert_form(&form, &[])
      .await
      .expect("insert child with string FK id");   // FAILS PRE-FIX on a real bigint col

  // read it back and prove the FK links the actual parent row
  let child = Child::objects().filter(child::ID.eq(child_id)).first().await
      .expect("fetch").expect("present");
  assert_eq!(child.parent.id(), parent.id, "stored FK must equal the seeded parent id");
  ```
  This is a round-trip (seed → submit string FK → read back → assert link), not a tautology. Pre-fix it fails (the bound TEXT trips the `INTEGER`/`bigint` column); post-fix it passes.
- [ ] Add `fk_id_as_json_string_via_insert_json` covering the *other* live path that hits the buggy arm — drive `insert_json` with `{"parent": "1", "body": "x"}` (FK id as a JSON **string**, the REST/admin JSON shape) and assert the same read-back link. This pins both reach-paths of `json_to_sea_value`'s FK arm.
- [ ] Run and CONFIRM RED: `cd crates && cargo test -p umbral-core --test fk_save_coercion`. Capture the exact error — it must read like #42 (a type-mismatch / `text` bind against a numeric column), proving the test drives the real bug and not a harness artifact. If it passes pre-fix, the harness isn't hitting the buggy arm — STOP and re-check that the FK id is submitted as a string and the column is declared numeric.

### Task 2 — Diagnostic probe: print the live FK column shape and resolved target PK

**Files:**
- `crates/umbral-core/tests/fk_save_coercion.rs` (NEW — add a `#[ignore]`-free diagnostic test, removed before commit)

Steps:

- [ ] Add a temporary diagnostic test `probe_fk_meta_shape` that boots, then for the `Child.parent` column prints the three values that disambiguate the suspects:
  ```rust
  let meta = ModelMeta::for_::<Child>();
  let col = meta.fields.iter().find(|c| c.name == "parent").expect("parent col");
  eprintln!("parent.ty            = {:?}", col.ty);          // Suspect B check
  eprintln!("parent.fk_target     = {:?}", col.fk_target);   // must be Some("fkparent")
  eprintln!("pk_meta(fkparent)    = {:?}", umbral::migrate::pk_meta_for_table("fkparent")); // Suspect C check
  ```
  (Use the facade path `umbral::migrate::pk_meta_for_table` if exported; otherwise `umbral_core::migrate::pk_meta_for_table`.)
- [ ] Run `cargo test -p umbral-core --test fk_save_coercion probe_fk_meta_shape -- --nocapture` and record the printed triple. Interpret:
  - `ty == ForeignKey`, `fk_target == Some("fkparent")`, `pk_meta == Some(("id", BigInt))` → **Suspect A** (the function that received the value couldn't act on `fk_target`). Expected outcome.
  - `ty != ForeignKey` (e.g. `BigInt`) → **Suspect B** (meta lost the FK marker). Falsified if A holds; branch retained for safety.
  - `pk_meta == None` while `fk_target == Some(...)` → **Suspect C** (`pk_meta_for_table` mis-resolves / registry timing). Would mean `fk_target_pk_sql_type` can't tell i64 from text.
- [ ] Add a *second* probe asserting the bound SeaValue directly, so the SQLite looseness can't mask the bug: build the value the live path produces and assert its variant.
  - For the `insert_form` path: `form_str_to_sea_value` is `pub(crate)` — call it in-crate via a tiny `#[cfg(test)]` re-export, OR assert through the public terminal by inspecting the generated SQL values. Simpler: assert via the JSON path, which is the confirmed buggy arm — `assert_eq!(json_to_sea_value(SqlType::ForeignKey, &json!("1"), false, "parent").unwrap(), SeaValue::BigInt(Some(1)))` **after** the fix; pre-fix this returns `SeaValue::String` and the assert documents the bug. (`json_to_sea_value` is `pub`.)
- [ ] Delete `probe_fk_meta_shape` before committing — it's a diagnostic, not a regression pin. The behavioral round-trip from Task 1 stays.

---

## Phase B — Fix the confirmed suspect (decision tree)

Pick the ONE branch the Task 2 probe confirmed. Each branch is real, complete fix code — not a placeholder. Suspect A is the expected branch; B and C are retained because the probe is the arbiter, not this document.

### Suspect A (EXPECTED) — `json_to_sea_value`'s FK arm binds TEXT because it can't see the target PK type

**Root cause:** `json_to_sea_value` (`write.rs:399`) binds `SqlType::ForeignKey` + `JsonValue::String` → `SeaValue::String` (TEXT) unconditionally (`write.rs:428-430`), with no `fk_target` in scope. A numeric-string FK id for an i64-PK parent is therefore bound as text. The fix gives the FK arm the target-PK context it needs and coerces accordingly.

**Files:**
- `crates/umbral-core/src/orm/write.rs:399` (`json_to_sea_value` signature + FK arm at `:428`)
- `crates/umbral-core/src/orm/dynamic.rs:1138` (`insert_json` call site), `:1346` region (`update_json` call site — find the matching `json_to_sea_value(col.ty, ...)` call)
- `crates/umbral-core/src/orm/queryset/write_helpers.rs:67` (typed `create` call site)

**The contract this restores:** *a `ForeignKey` value coerces against its target PK's `SqlType` — numeric-PK target ⇒ parse string → `BigInt`; Text/Uuid-PK target ⇒ bind string as-is.* This mirrors what `form_str_to_sea_value` (`dynamic.rs:1954`) already does via `fk_target_pk_sql_type`. We unify on that resolution instead of guessing from the JSON value's type.

Steps:

- [ ] Add an optional target-PK hint to `json_to_sea_value`. Prefer threading the resolved PK type rather than the whole `Column` (keeps the function pure and callable from `write.rs` which can't depend on `dynamic.rs`'s `Column`):
  ```rust
  pub fn json_to_sea_value(
      sql_type: SqlType,
      value: &JsonValue,
      nullable: bool,
      field_name: &str,
      fk_target_pk: Option<SqlType>,   // NEW: the FK target's PK SqlType, None for non-FK
  ) -> Result<SeaValue, WriteError> {
  ```
- [ ] Rewrite the FK arm (`write.rs:428-430`) to honor the hint:
  ```rust
  SqlType::ForeignKey => match fk_target_pk {
      // Text / Uuid PK target: bind the id as text / uuid.
      Some(SqlType::Text) => coerce_string(value, field_name)
          .map(|s| SeaValue::String(Some(Box::new(s)))),
      Some(SqlType::Uuid) => match value {
          JsonValue::String(s) => uuid::Uuid::parse_str(s)
              .map(|u| SeaValue::Uuid(Some(Box::new(u))))
              .map_err(|_| WriteError::TypeMismatch {
                  field: field_name.to_string(), expected: SqlType::Uuid, got: s.clone(),
              }),
          _ => coerce_i64(value, field_name).map(|v| SeaValue::BigInt(Some(v))),
      },
      // Numeric PK target (or unresolved): coerce a numeric string / number → i64.
      // coerce_i64 already accepts JsonValue::String("1") and JsonValue::Number.
      _ => coerce_i64(value, field_name).map(|v| SeaValue::BigInt(Some(v))),
  },
  ```
  Note `coerce_i64` (`write.rs:785`) already parses `JsonValue::String` → i64, so a string `"1"` now binds `BigInt(1)` for an i64 target. The `None` (unresolved target) case defaults to BigInt — matching `form_str_to_sea_value`'s existing default and the overwhelmingly-common i64-PK case.
- [ ] Update all `json_to_sea_value` call sites to pass the hint:
  - `dynamic.rs` `insert_json` (`:1138`) and `update_json`: pass `fk_target_pk_sql_type(col)` (already in this module, `:1699`) so each FK column resolves its target PK. For non-FK columns it returns `None`, which the new arm ignores.
    ```rust
    let sea_value = crate::orm::write::json_to_sea_value(
        col.ty, json, col.nullable, &col.name, fk_target_pk_sql_type(col),
    )?;
    ```
  - `write_helpers.rs` `build_insert_one_for` (`:67`) and `build_insert_many_for`: resolve from the typed `FieldSpec`. `FieldSpec.fk_target: Option<&'static str>` is available; resolve via `crate::migrate::pk_meta_for_table(field.fk_target?).map(|(_, ty)| ty)`. Inline a tiny helper:
    ```rust
    fn fk_pk_hint(field: &crate::orm::FieldSpec) -> Option<SqlType> {
        field.fk_target.and_then(|t| crate::migrate::pk_meta_for_table(t).map(|(_, ty)| ty))
    }
    ```
    then `json_to_sea_value(field.ty, &val, field.nullable, field.name, fk_pk_hint(field))`.
  - The in-crate unit tests at `write.rs:913+` and any other caller: add a trailing `None` argument (non-FK cases) so they compile.
- [ ] OPTIONAL consolidation (do only if it falls out cleanly): `form_str_to_sea_value`'s FK arm (`dynamic.rs:1954`) duplicates this resolution. Once `json_to_sea_value` takes the hint, the FK branch in `form_str_to_sea_value` can delegate: `return json_to_sea_value(SqlType::ForeignKey, &serde_json::Value::String(raw.to_string()), col.nullable, &col.name, fk_target_pk_sql_type(col));`. This removes the parallel implementation so the two can't drift. Keep the existing helper tests green; if delegation changes any error message, prefer NOT consolidating in this commit (log it as a follow-up gap) over churning error text.

### Suspect B — the live meta lost `SqlType::ForeignKey` (FK column arrives as `BigInt`)

**Confirm-by:** Task 2 probe prints `col.ty != ForeignKey` for `Child.parent`.

**Root cause (if confirmed):** somewhere between the derive's `FieldSpec` and the admin's `ModelMeta`, the FK column's `ty` is rewritten to `BigInt` (so `form_str_to_sea_value`'s FK arm at `:1954` is skipped and it falls through to `json_to_sea_value` with `BigInt` — which *would* bind correctly, so this suspect actually points at a DIFFERENT downstream TEXT source; investigate where a `BigInt` column produces a string bind, e.g. an `inspectdb`-derived meta the admin uses).

**Files:**
- `crates/umbral-macros/src/lib.rs:2098` (the `ty` token emission) — verify the FK arm emits `SqlType::ForeignKey`.
- `crates/umbral-core/src/migrate.rs:834` (`From<&FieldSpec> for Column`) — verify `ty: f.ty` is copied, not normalized.
- `crates/umbral-core/src/inspect.rs:848` (the `inspectdb` path that builds columns from DB introspection) — a DB-introspected FK comes back as `INTEGER`/`BigInt` with `fk_target: None`, losing the marker.

Steps:

- [ ] Trace which meta the failing path uses (registered-model meta vs inspect-derived). If it's the registered-model meta, this suspect is falsified (the derive emits `ForeignKey`); re-confirm Suspect A.
- [ ] If the admin uses an inspect-derived meta: fix `inspect.rs` to set `ty = SqlType::ForeignKey` and `fk_target = Some(referenced_table)` when introspection finds a `REFERENCES` constraint, so the FK marker survives. Then Suspect A's coercion (already correct for `ForeignKey` + resolved PK) handles the bind.

### Suspect C — `fk_target_pk_sql_type` mis-resolves the target PK type

**Confirm-by:** Task 2 probe prints `fk_target == Some("fkparent")` but `pk_meta_for_table("fkparent") == None` (or a wrong `SqlType`).

**Root cause (if confirmed):** `pk_meta_for_table` (`migrate.rs:121`) returns `None` before `App::build` populates the registry, or caches an incomplete map, so `fk_target_pk_sql_type` (`dynamic.rs:1699`) returns `None` and downstream code can't tell an i64 FK from a text FK. With Suspect A's fix the `None` case still defaults to BigInt (correct for the common case), but a String/Uuid-PK target would mis-bind.

**Files:**
- `crates/umbral-core/src/migrate.rs:121` (`pk_meta_for_table`), `:128` (the `CACHE` `OnceLock`)

Steps:

- [ ] Confirm the registry is initialised at the failing call time (`is_initialised()` true). If the cache memoized while the target model wasn't yet registered, the `OnceLock` froze an incomplete map — the existing guard at `:122` only blocks the *uninitialised* case, not a *partially-initialised* one.
- [ ] If partial-init is the cause: make the cache keyed on registry generation, or rebuild it if a probed table is absent:
  ```rust
  let map = CACHE.get_or_init(|| { /* build from registered_models() */ });
  if let Some(hit) = map.get(table) { return Some(hit.clone()); }
  // Absent: the cache may predate this model's registration — fall back to a live scan
  registered_models().into_iter()
      .find(|m| m.table == table)
      .and_then(|m| m.pk_column().map(|pk| (pk.name.clone(), pk.ty)))
  ```
  This keeps the hot-path cache while never returning `None` for a table that IS registered.

---

## Phase C — Regression pin + parallel typed-path assertion

### Task 3 — Lock the fix and prevent the two write paths from diverging

**Files:**
- `crates/umbral-core/tests/fk_save_coercion.rs` (extend with the parallel typed-path assertion)

Steps:

- [ ] Keep Task 1's behavioral round-trips green (`fk_id_as_string_round_trips_and_links_parent`, `fk_id_as_json_string_via_insert_json`). Re-run and CONFIRM GREEN post-fix.
- [ ] Add the **parallel typed-path assertion** — the typed `create` path must bind the FK the same way as the dynamic path, so a future change to either can't silently reintroduce the text bind:
  ```rust
  #[tokio::test]
  async fn typed_create_binds_fk_as_bigint_same_as_dynamic() {
      boot().await;
      let parent = Parent::objects().create(Parent { id: 0, name: "Acme".into() }).await.unwrap();

      // Typed path: ForeignKey<Parent> constructed from the i64 id.
      let child = Child::objects()
          .create(Child { id: 0, parent: ForeignKey::from_id(parent.id), body: "typed".into() })
          .await
          .expect("typed create with FK");
      let back = Child::objects().filter(child::ID.eq(child.id)).first().await.unwrap().unwrap();
      assert_eq!(back.parent.id(), parent.id, "typed-path FK must link the real parent");
  }
  ```
  (Use whatever the real `ForeignKey<T>` constructor is — `ForeignKey::from_id` / `ForeignKey::new` / `parent.id.into()`; check `crates/umbral-core/src/orm` for the exact API and match it.)
- [ ] Add a direct-bind invariant test asserting BOTH paths funnel through the same coercion, so the assertion can't be satisfied by accident:
  ```rust
  #[test]
  fn json_fk_arm_coerces_numeric_string_to_bigint() {
      // The unit-level pin on the exact arm that #42 mis-bound.
      // Without the registry, fk_target_pk hint is None → defaults to BigInt.
      let v = umbral::orm::write::json_to_sea_value(
          SqlType::ForeignKey, &serde_json::json!("1"), false, "parent", None,
      ).unwrap();
      assert_eq!(v, SeaValue::BigInt(Some(1)),
          "REGRESSION (gaps2 #42): a string FK id for a numeric-PK target must bind BigInt, not TEXT");
      // And a resolved Text-PK target still binds text.
      let t = umbral::orm::write::json_to_sea_value(
          SqlType::ForeignKey, &serde_json::json!("perm.add"), false, "perm", Some(SqlType::Text),
      ).unwrap();
      assert!(matches!(t, SeaValue::String(_)), "Text-PK FK must still bind text");
  }
  ```
- [ ] Confirm the existing helper tests still pass: `dynamic.rs:2606` (`form_fk_numeric_string_binds_as_bigint`), `:2619` (nullable blank), and `write.rs:913+`. If the Suspect A consolidation changed any signature, they must compile and pass unchanged in behavior.

### Task 4 — Full-workspace verification + gap closure + commit

**Files:**
- `planning/gaps2.md` (shrink #42 to a one-line `[x] ... — archived` stub in place; do NOT renumber or move it)
- `planning/archive/gaps2-done.md` (append #42's full write-up verbatim under the same number)

Steps:

- [ ] `cd crates && cargo fmt && cargo clippy --all-targets && cargo build && cargo test` — the WHOLE workspace, not just `umbral-core` (a `write.rs` signature change ripples to every `json_to_sea_value` caller across crates; the facade re-export and plugins must still compile). Fix anything red; never `--no-verify`.
- [ ] Run the umbral-admin tests specifically (the original failing consumer surface): `cargo test -p umbral-admin`. They must pass — the admin's `insert_row`/`update_row` → `insert_form`/`update_form` path now binds FKs correctly.
- [ ] Close gaps2 #42: move the full shipped write-up to `planning/archive/gaps2-done.md` under #42; leave a one-line `[x] FK save binds text not bigint — archived` stub in `planning/gaps2.md` (per the gap-tracker convention: same number, stub stays in place, full text moves).
- [ ] Run `gitnexus_detect_changes()` and confirm only the expected symbols/flows changed (the `json_to_sea_value` signature + its callers).
- [ ] Commit as ONE logical change:
  ```
  fix(orm): bind FK ids against the target PK type, not text (gaps2 #42)

  json_to_sea_value's ForeignKey arm bound every string-valued FK id as
  TEXT because the function couldn't see the target's PK type — so a
  numeric-PK FK submitted as a string ("1", the form/JSON shape) hit
  `column "..." is of type bigint but expression is of type text`. Thread
  the resolved target-PK SqlType into the coercion so a numeric-PK FK
  parses string -> BigInt while String/Uuid-PK targets still bind as-is.
  The typed create path already serialized FK ids as numbers; a parallel
  regression test pins both paths to the same coercion so they can't drift.

  Closes gaps2 #42.

  Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
  ```

---

## Why this is diagnosis-first and not a guess

The helper-level FK arm (`form_str_to_sea_value`, `dynamic.rs:1954`) is correct and unit-tested, so the bug is provably on a *different* path. Reading the call graph identifies one structural defect — `json_to_sea_value` cannot consult `fk_target` and so binds string FKs as TEXT — reached by both the admin JSON write path and (latently) the typed create path. The Phase A probe is what turns "identifies" into "confirms," printing the live `col.ty` / `fk_target` / resolved target-PK so Phase B applies the matching branch with certainty rather than hope. The fix targets the contract (*FK binds against its target PK type, never raw text*), not the symptom, and Phase C's parallel assertion makes the dynamic and typed paths share one coercion so #42 cannot silently return.

## Suspect list (the Phase B decision tree)

- **A (expected):** `json_to_sea_value`'s `ForeignKey` arm (`write.rs:428-430`) binds `JsonValue::String` → `SeaValue::String` (TEXT) with no `fk_target` in scope. Fix: thread the target-PK `SqlType` hint into the function and coerce numeric-PK FKs string→BigInt. Reached by `insert_json`/`update_json` (`dynamic.rs:1138`) and `build_insert_one_for` (`write_helpers.rs:67`).
- **B:** the live `ModelMeta` lost `SqlType::ForeignKey` (FK column arrives as `BigInt`/`fk_target: None`), most plausibly via an `inspectdb`-derived meta (`inspect.rs:848`). Fix: preserve the FK marker + `fk_target` through introspection.
- **C:** `pk_meta_for_table` (`migrate.rs:121`) mis-resolves the target PK (partial-init cache freeze) so `fk_target_pk_sql_type` returns `None`/wrong type. Fix: live-scan fallback when a registered table is absent from the cached map.
