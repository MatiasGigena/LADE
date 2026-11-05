#[path = "../tests/support/mod.rs"]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code fails loudly on broken fixtures"
)]
mod support;

fn main() -> serde_json::Result<()> {
    println!("{}", serde_json::to_string(&support::report())?);
    Ok(())
}
