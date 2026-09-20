#[test]
fn decision_builders_default_to_cacheable() {
    let d = umbral_storage::Decision::of(true).depends_on(["chan:1".to_string()]);
    assert!(d.is_allow());
    assert!(d.is_cacheable());
    assert_eq!(d.tags(), &["chan:1".to_string()]);
    let n = umbral_storage::Decision::allow().no_cache();
    assert!(!n.is_cacheable());
}
