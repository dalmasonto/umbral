//! TDD: verify `email_verified_at` field on `AuthUser` and the new
//! `AuthChallenge` model structure — before and independent of any
//! migration or DB connection.

#[test]
fn auth_user_has_email_verified_at_and_challenge_table_registered() {
    use umbral::migrate::ModelMeta;
    let user = ModelMeta::for_::<umbral_auth::AuthUser>();
    assert!(
        user.fields
            .iter()
            .any(|c| c.name == "email_verified_at" && c.nullable),
        "AuthUser must expose a nullable email_verified_at column"
    );
    let ch = ModelMeta::for_::<umbral_auth::AuthChallenge>();
    assert_eq!(ch.table, "auth_challenge");
    for f in [
        "user_id",
        "purpose",
        "secret_hash",
        "expires_at",
        "attempts",
        "used_at",
        "created_at",
    ] {
        assert!(
            ch.fields.iter().any(|c| c.name == f),
            "AuthChallenge missing column {f}"
        );
    }
}
