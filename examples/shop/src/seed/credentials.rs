//! First-run convenience: mints a deterministic superuser
//! ("shopadmin" / "shopadmin") plus a bearer token when no
//! users exist yet, printing both to stderr so the developer
//! can curl the gated endpoints without leaving the terminal.
//! Idempotent — subsequent boots find the user and stay quiet.
//!
//! NEVER ship this in production. It's a test scaffold;
//! production would call `createsuperuser` interactively.

use umbral_auth::AuthUser;
use umbral_auth::token::AuthToken;

pub async fn test_credentials() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if AuthUser::objects().count().await? > 0 {
        return Ok(());
    }

    let user = umbral_auth::create_superuser("shopadmin", "shopadmin@example.com", "shopadmin")
        .await
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
    let (_row, token) = AuthToken::create_for(&user, "shop-demo")
        .await
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;

    eprintln!();
    eprintln!("======================================================================");
    eprintln!(" TEST CREDENTIALS — seeded because no users existed yet (gap #82)");
    eprintln!("----------------------------------------------------------------------");
    eprintln!(" Username : shopadmin");
    eprintln!(" Password : shopadmin");
    eprintln!(" Token    : {token}");
    eprintln!();
    eprintln!(" Try a public read (no auth):");
    eprintln!("   curl -s http://localhost:8000/api/product/?fields=id,name,price | jq");
    eprintln!();
    eprintln!(" Try a Bearer-gated write:");
    eprintln!("   curl -X DELETE -H 'Authorization: Bearer {token}' \\");
    eprintln!("        http://localhost:8000/api/product/1");
    eprintln!();
    eprintln!(" Try the CUSTOM `Token` scheme on a blog post:");
    eprintln!("   curl -X DELETE -H 'Authorization: Token {token}' \\");
    eprintln!("        http://localhost:8000/api/post/1");
    eprintln!();
    eprintln!(" Try anonymous write (expect 401):");
    eprintln!("   curl -X DELETE http://localhost:8000/api/product/1");
    eprintln!();
    eprintln!(" Try order (any auth required, even read):");
    eprintln!("   curl -H 'Authorization: Bearer {token}' \\");
    eprintln!("        http://localhost:8000/api/order/");
    eprintln!("======================================================================");
    eprintln!();

    Ok(())
}
