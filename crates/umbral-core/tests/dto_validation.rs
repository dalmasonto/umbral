//! Request-body validation for non-model DTOs (gaps3 #29 item 4).
//!
//! The live consumer hand-wrote the same four rules across 12 handlers — enum-of-
//! strings ×3, trim ×4, non-empty ×3, min-length ×2 — because `#[umbral(trim, ...)]`
//! only worked on a `Model`, and a request body is usually not a model.
//!
//! Two claims are worth testing, and only one of them is "validation happens":
//!
//! 1. `Valid<T>` puts the gate in the handler's SIGNATURE, and rejects with the same
//!    structured 400 REST already emits for a model write.
//! 2. A DTO and a `Model` agree on what a valid value IS. Two validators that both
//!    "check emails" is how an API ends up accepting a value its own database rejects.

use axum::Router;
use axum::body::Body;
use axum::routing::post;
use http::{Request, StatusCode};
use serde::{Deserialize, Serialize};
use tower::ServiceExt;
use umbral::validate::{Valid, Validate, ValidationErrors};

#[derive(Debug, Clone, Deserialize, Serialize, umbral::validate::Validate)]
struct CreateGoal {
    #[umbral(trim, min_length = 1, max_length = 12)]
    scorer: String,
    #[umbral(choices = ["home", "away"])]
    side: String,
    #[umbral(min = 0, max = 120)]
    minute: i64,
    #[umbral(trim, lowercase, email)]
    notify: Option<String>,
}

async fn handler(Valid(body): Valid<CreateGoal>) -> axum::Json<CreateGoal> {
    // If we are here, the body is normalised AND checked. There is no path into this
    // function where it is not — that is the whole point of doing it in the signature.
    axum::Json(body)
}

fn app() -> Router {
    Router::new().route("/goal", post(handler))
}

async fn post_json(body: serde_json::Value) -> (StatusCode, serde_json::Value) {
    let res = app()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/goal")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

/// The happy path also proves the NORMALISERS ran: the body goes in with whitespace
/// and a shouty email, and comes back trimmed and lowercased. A validator that could
/// only say "no" would leave every caller to normalise by hand — which is the
/// boilerplate this exists to delete.
#[tokio::test]
async fn a_valid_body_is_normalised_in_place() {
    let (status, body) = post_json(serde_json::json!({
        "scorer": "  Ada  ",
        "side": "home",
        "minute": 41,
        "notify": "  Ada@Example.COM  ",
    }))
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["scorer"], "Ada", "trim rewrote the value");
    assert_eq!(
        body["notify"], "ada@example.com",
        "trim + lowercase applied inside the Option"
    );
}

/// Every broken rule is reported, not just the first. Making a user fix one mistake
/// per round-trip is its own kind of bug.
#[tokio::test]
async fn every_broken_rule_is_reported_at_once() {
    let (status, body) = post_json(serde_json::json!({
        "scorer": "",
        "side": "sideways",
        "minute": 500,
        "notify": "not-an-email",
    }))
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "validation_error");

    // The 400 shape is byte-for-byte REST's: field errors FLATTENED to the top level,
    // keyed by field name. A client should not have to care whether the 400 came from
    // a viewset or a hand-written handler.
    for field in ["scorer", "side", "minute", "notify"] {
        assert!(
            body.get(field).is_some(),
            "every failing field must appear; `{field}` is missing from {body}"
        );
    }
    assert!(
        body["side"][0].as_str().unwrap().contains("home"),
        "the error should name the allowed values, got {}",
        body["side"]
    );
}

/// The trap this ordering closes: normalisers run BEFORE checks, so a whitespace-only
/// string is caught as blank rather than sailing through as "non-empty".
#[tokio::test]
async fn whitespace_only_is_blank_because_trim_runs_first() {
    let (status, body) = post_json(serde_json::json!({
        "scorer": "     ",
        "side": "away",
        "minute": 10,
        "notify": null,
    }))
    .await;

    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "`     ` must be blank, not a 5-character name"
    );
    assert!(body["scorer"][0].as_str().unwrap().contains("blank"));
}

/// A `None` optional is not a validation failure — the rules describe the value, and
/// there is no value. Getting this backwards would make every optional field required.
#[tokio::test]
async fn an_absent_optional_skips_its_rules() {
    let (status, _) = post_json(serde_json::json!({
        "scorer": "Grace",
        "side": "away",
        "minute": 90,
        "notify": null,
    }))
    .await;
    assert_eq!(status, StatusCode::OK);
}

/// A body that will not even parse has no field-level shape to report, and must not be
/// dressed up as one.
#[tokio::test]
async fn a_malformed_body_is_not_a_field_error() {
    let (status, body) = post_json(serde_json::json!({"scorer": 7})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "malformed_body");
}

/// The claim that makes this worth having rather than merely present: a DTO and a
/// `Model` share ONE definition of each rule. `#[umbral(email)]` on a DTO calls the
/// same `validate_text_format` the ORM's write path calls, so a string the API accepts
/// is a string the database accepts.
#[test]
fn a_dto_and_a_model_agree_on_what_a_valid_email_is() {
    #[derive(Deserialize, umbral::validate::Validate)]
    struct Dto {
        #[umbral(email)]
        email: String,
    }

    for candidate in ["ada@example.com", "not-an-email", "@nope", "a@b.c"] {
        let mut dto = Dto {
            email: candidate.to_string(),
        };
        let dto_ok = dto.validate().is_ok();
        let orm_ok = umbral::validate::validate_text_format("email", candidate).is_ok();
        assert_eq!(
            dto_ok, orm_ok,
            "DTO and ORM disagree about `{candidate}` — that divergence is how an API \
             accepts a value its own database will reject"
        );
    }
}

/// Errors accumulate through the public `ValidationErrors` type the forms layer already
/// uses, so an admin form, a `Form<T>` post and a JSON body all speak one error shape.
#[test]
fn errors_land_in_the_shared_validation_errors_type() {
    let mut dto = CreateGoal {
        scorer: "waaaaaaaaaaaaay too long".to_string(),
        side: "home".to_string(),
        minute: 5,
        notify: None,
    };
    let errs: ValidationErrors = dto.validate().expect_err("too long");
    assert_eq!(errs.fields.len(), 1);
    assert!(errs.fields["scorer"][0].contains("at most 12"));
}
