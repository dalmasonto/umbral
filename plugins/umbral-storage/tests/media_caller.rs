use http::HeaderMap;
use umbral_storage::MediaCaller;

#[tokio::test]
async fn anonymous_headers_resolve_to_unauthenticated_caller() {
    let caller = MediaCaller::resolve(&HeaderMap::new()).await;
    assert!(!caller.is_authenticated());
    assert_eq!(caller.user_id(), None);
    assert!(!caller.is_superuser);
    assert!(caller.roles.is_empty());
}
