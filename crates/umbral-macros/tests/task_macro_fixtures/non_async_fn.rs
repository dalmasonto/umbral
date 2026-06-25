use umbral_tasks::EnqueueOptions;

#[derive(serde::Deserialize)]
struct MyPayload {
    value: i64,
}

#[umbral::task]
fn not_async(payload: MyPayload) -> Result<(), String> {
    let _ = payload;
    Ok(())
}

fn main() {}
