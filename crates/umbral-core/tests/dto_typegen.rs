//! `#[derive(Dto)]` — hand-shaped response structs in the generated client (gaps3 #29.5).
//!
//! `typegen` / `gen-client` emit TypeScript from `ModelMeta`, which covers everything
//! except the shape you actually wrote — the moment one handler returns
//! `Json<MemberCard>`, the generated client is missing precisely the type you needed it
//! for. So you hand-write the lot and stop generating. That is exactly what the live
//! consumer did: three hand-shaped response structs, a raw `HashMap`, and `gen-client`
//! adopted not at all.

use serde::Serialize;
use umbral::typegen::{Dto, registered_dtos, typescript_for_dtos};

/// A member's summary card.
#[derive(Serialize, Dto)]
#[allow(dead_code)]
struct MemberCard {
    /// Display name.
    name: String,
    matches_played: i64,
    rating: f64,
    active: bool,
    last_seen: Option<String>,
    positions: Vec<String>,
    nicknames: Option<Vec<String>>,
    stats: std::collections::HashMap<String, i64>,
    raw: serde_json::Value,
    /// Nested DTOs resolve to their own interface by name.
    club: ClubRef,
}

#[derive(Serialize, Dto)]
#[allow(dead_code)]
struct ClubRef {
    id: i64,
    name: String,
}

#[derive(Serialize, Dto)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct CamelPayload {
    member_id: i64,
    total_goals_scored: i64,
    #[serde(rename = "explicitlyNamed")]
    whatever: String,
    #[serde(skip)]
    internal_only: String,
}

fn ts_for(name: &str) -> String {
    let dtos = registered_dtos();
    let one: Vec<_> = dtos.into_iter().filter(|d| d.name == name).collect();
    assert_eq!(one.len(), 1, "`{name}` should be registered exactly once");
    typescript_for_dtos(&one)
}

/// A DTO is in the client by virtue of EXISTING. Link-time registration (the same
/// `inventory` mechanism `#[derive(Model)]` uses) means there is no registry to
/// remember to add it to — which is the failure mode every registry has.
#[test]
fn a_dto_registers_itself_at_link_time() {
    let names: Vec<&str> = registered_dtos().iter().map(|d| d.name).collect();
    for expected in ["CamelPayload", "ClubRef", "MemberCard"] {
        assert!(names.contains(&expected), "missing {expected} in {names:?}");
    }
}

/// The Rust → TypeScript mapping, across every shape a real response body uses.
#[test]
fn rust_types_map_to_the_typescript_a_client_can_use() {
    let ts = ts_for("MemberCard");

    assert!(ts.contains("export interface MemberCard {"), "{ts}");
    assert!(ts.contains("name: string;"), "{ts}");
    assert!(ts.contains("matches_played: number;"), "{ts}");
    assert!(ts.contains("rating: number;"), "{ts}");
    assert!(ts.contains("active: boolean;"), "{ts}");
    assert!(ts.contains("positions: string[];"), "{ts}");
    assert!(ts.contains("stats: Record<string, number>;"), "{ts}");

    // `Option<T>` is both optional AND nullable: serde omits `None` under
    // `skip_serializing_if`, and emits `null` without it. The type has to admit both,
    // or the client breaks on whichever one it did not expect.
    assert!(ts.contains("last_seen?: string | null;"), "{ts}");
    assert!(ts.contains("nicknames?: string[] | null;"), "{ts}");

    // `unknown`, not `any`. The consumer must narrow it — which is the entire point of
    // generating types at all.
    assert!(ts.contains("raw: unknown;"), "{ts}");

    // A nested DTO is referenced by its own interface name, and that interface exists.
    assert!(ts.contains("club: ClubRef;"), "{ts}");
    assert!(
        ts_for("ClubRef").contains("export interface ClubRef {"),
        "the nested type must actually be emitted, not just referenced"
    );

    // Doc-comments carry through — a generated type nobody can read is a type nobody
    // trusts.
    assert!(ts.contains("Display name."), "{ts}");
}

/// **The one that matters.** `#[serde(rename_all = "camelCase")]` means the server sends
/// `memberId`. A generated client that emitted `member_id` would compile, type-check,
/// and be wrong at runtime against the very server that produced it — the worst failure
/// available, because nothing errors.
#[test]
fn serde_rename_all_is_honoured_not_ignored() {
    let ts = ts_for("CamelPayload");

    assert!(ts.contains("memberId: number;"), "{ts}");
    assert!(ts.contains("totalGoalsScored: number;"), "{ts}");
    assert!(
        !ts.contains("member_id"),
        "the Rust field name leaked into the client: {ts}"
    );

    // A field-level rename wins over the struct-level style.
    assert!(ts.contains("explicitlyNamed: string;"), "{ts}");

    // `#[serde(skip)]` → not in the JSON → not in the TypeScript.
    assert!(
        !ts.contains("internal_only") && !ts.contains("internalOnly"),
        "a skipped field must not appear in the client: {ts}"
    );
}
