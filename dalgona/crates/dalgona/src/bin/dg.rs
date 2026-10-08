// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! The `dg` binary entry point.

fn main() -> std::process::ExitCode {
    dalgon::run(dalgona::product())
}
