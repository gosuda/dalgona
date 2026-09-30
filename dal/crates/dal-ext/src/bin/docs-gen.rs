//! Manual generator binary.
fn main() -> std::process::ExitCode {
    std::process::ExitCode::from(u8::try_from(dal_ext::docsgen::cli(std::env::args())).unwrap_or(2))
}
