// Migrations are embedded by `sqlx::migrate!`; rebuild when they change.
fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
