//! Manual generator binary.
#[path = "../docsgen.rs"]
pub mod docsgen;

fn main() -> std::process::ExitCode {
    std::process::ExitCode::from(u8::try_from(docsgen::cli(std::env::args())).unwrap_or(2))
}
