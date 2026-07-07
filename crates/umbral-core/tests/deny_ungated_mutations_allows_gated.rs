//! gaps3 #28 P1 — the companion to `deny_ungated_mutations_rejects.rs`: a
//! mutating route whose permission IS recorded (via core's `Routes::route_gated`)
//! builds cleanly even with `deny_ungated_mutations()` set. This proves the
//! strict flag gates on the *recorded permission*, not on the HTTP method — a
//! properly gated POST is not a false positive.

use axum::routing::post;
use umbral_core::app::App;
use umbral_core::db;
use umbral_core::routes::Routes;
use umbral_core::settings::Settings;

#[tokio::test]
async fn gated_post_builds_under_strict_flag() {
    let settings = Settings::from_env().expect("figment defaults always load");
    let pool = db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite should always connect");

    let result = App::builder()
        .settings(settings)
        .database("default", pool)
        .routes(
            Routes::new()
                .get("/", || async { "home" })
                // route_gated records the permission on the RouteSpec, so the
                // H19 audit sees this POST as gated.
                .route_gated(
                    &["POST"],
                    "/posts",
                    post(|| async { "created" }),
                    "blog.add",
                ),
        )
        .deny_ungated_mutations()
        .build();

    assert!(
        result.is_ok(),
        "a gated mutating route must build under deny_ungated_mutations(); got {:?}",
        result.err(),
    );
}
