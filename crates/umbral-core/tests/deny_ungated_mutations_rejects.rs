//! gaps3 #28 P1 — `AppBuilder::deny_ungated_mutations()` promotes the audit_2
//! H19 boot *warning* into a hard `BuildError`. An app-level mutating route
//! (POST/PUT/PATCH/DELETE) registered via `.routes(...)` with NO recorded
//! permission must fail `build()` when the strict flag is set.
//!
//! `build()` initializes process-wide `OnceLock`s, so each scenario lives in
//! its own test binary (see `builder.rs` for the rationale). This file proves
//! the reject path; `deny_ungated_mutations_allows_gated.rs` proves a gated
//! route still builds under the same flag.

use umbral_core::app::{App, BuildError};
use umbral_core::db;
use umbral_core::routes::Routes;
use umbral_core::settings::Settings;

#[tokio::test]
async fn ungated_post_fails_build_under_strict_flag() {
    let settings = Settings::from_env().expect("figment defaults always load");
    let pool = db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite should always connect");

    let result = App::builder()
        .settings(settings)
        .database("default", pool)
        // A read route is fine; the ungated POST is the offender.
        .routes(
            Routes::new()
                .get("/", || async { "home" })
                .post("/contact", || async { "sent" }),
        )
        .deny_ungated_mutations()
        .build();

    match result {
        Err(BuildError::UngatedMutatingRoutes { routes }) => {
            assert_eq!(
                routes,
                vec!["POST /contact".to_string()],
                "the error must name exactly the ungated mutating route"
            );
            // The Display impl points the operator at the fix.
            let msg = BuildError::UngatedMutatingRoutes { routes }.to_string();
            assert!(
                msg.contains("POST /contact") && msg.contains("require_permission"),
                "Display must name the route and the fix; got {msg}"
            );
        }
        Err(other) => panic!(
            "expected Err(UngatedMutatingRoutes), got a different BuildError: {other}; \
             deny_ungated_mutations() must turn the H19 warning into this specific error"
        ),
        Ok(_) => panic!(
            "expected build() to fail; deny_ungated_mutations() must turn the H19 warning \
             into a build error for an ungated POST route"
        ),
    }
}
