// `#[umbral::model(...)]` takes exactly `base = <Base>`; anything else is a
// clear parse error rather than a silent no-op.
#[umbral::model(nonsense)]
#[derive(Debug, umbral::orm::Model)]
struct Bad {
    id: i64,
}

fn main() {}
