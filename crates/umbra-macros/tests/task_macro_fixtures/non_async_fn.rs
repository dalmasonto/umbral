use umbra_tasks::EnqueueOptions;

#[derive(serde::Deserialize)]
struct MyPayload {
    value: i64,
}

#[umbra::task]
fn not_async(payload: MyPayload) -> Result<(), String> {
    let _ = payload;
    Ok(())
}

fn main() {}
