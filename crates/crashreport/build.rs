//! `BUILD_DSN` is `option_env!("SENTRY_DSN")`, which cargo bakes in at compile
//! time. Without this line cargo has no reason to rebuild when the variable
//! changes, so exporting a DSN and rebuilding would produce a binary that still
//! carries the old answer — a build that silently reports to nowhere (or, worse,
//! to somewhere stale) while looking correctly configured.
fn main() {
    println!("cargo:rerun-if-env-changed=SENTRY_DSN");
}
