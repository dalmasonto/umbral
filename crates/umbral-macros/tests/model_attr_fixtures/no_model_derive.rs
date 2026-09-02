// `#[umbral::model(base = ...)]` with no `#[derive(..., Model)]` below it —
// the classic ordering footgun (derive written above the attribute, or
// omitted). The self-check turns this into a clear message instead of a
// downstream "missing primary key" error.
#[umbral::model(base = TimeStamped)]
#[derive(Debug, Clone)]
struct Bad {
    x: i32,
}

fn main() {}
