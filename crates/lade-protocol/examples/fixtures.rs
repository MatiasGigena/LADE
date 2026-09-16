#[path = "../tests/support/mod.rs"]
mod support;

fn main() {
    println!("{}", serde_json::to_string(&support::report()).unwrap());
}
