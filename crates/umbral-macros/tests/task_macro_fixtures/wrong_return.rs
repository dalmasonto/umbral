#[derive(serde::Deserialize)]
struct Payload { value: i64 }

#[umbral::task]
async fn wrong_return(payload: Payload) -> Result<(), Box<dyn std::error::Error>> {
    let _ = payload;
    Ok(())
}

fn main() {}
