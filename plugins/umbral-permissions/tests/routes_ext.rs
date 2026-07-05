//! audit_2 H19 — the `RoutesPermExt` gated builders record the permission in
//! the `RouteSpec` (so the boot audit sees the route as gated) while also
//! applying the `permission_required` enforcement layer.

use umbral::routes::Routes;
use umbral_permissions::RoutesPermExt;

async fn handler() -> &'static str {
    "ok"
}

#[test]
fn gated_builders_record_permission_ungated_do_not() {
    let routes = Routes::new()
        .post("/open", handler) // plain builder → no recorded permission
        .post_gated("/posts", handler, "blog.add_post")
        .delete_gated("/posts/{id}", handler, "blog.delete_post");

    let (_router, specs) = routes.into_parts();

    let open = specs
        .iter()
        .find(|s| s.path == "/open")
        .expect("/open spec");
    assert!(
        open.permission.is_none(),
        "a plain .post() records no permission → the audit flags it"
    );

    let create = specs
        .iter()
        .find(|s| s.path == "/posts")
        .expect("/posts spec");
    assert_eq!(create.permission.as_deref(), Some("blog.add_post"));
    assert_eq!(create.methods, vec!["POST"]);

    let delete = specs
        .iter()
        .find(|s| s.path == "/posts/{id}")
        .expect("/posts/{id} spec");
    assert_eq!(delete.permission.as_deref(), Some("blog.delete_post"));
    assert_eq!(delete.methods, vec!["DELETE"]);
}
