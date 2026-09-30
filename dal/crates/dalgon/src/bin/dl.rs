//! The `dl` binary alias over the shared process edge.
#![forbid(unsafe_code)]

fn main() -> std::process::ExitCode {
    dalgon::run(dalgon::product())
}
