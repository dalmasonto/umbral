#[derive(serde::Deserialize)]
struct Payload { value: i64 }

#[umbra::task]
async fn two_params(a: Payload, b: String) -> Result<(), String> {
    let _ = (a, b);
    Ok(())
}

fn main() {}
