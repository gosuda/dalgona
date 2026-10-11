// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! The `dg` alias of the dalgona binary.

fn main() -> std::process::ExitCode {
    dalgon::run(dalgona::product())
}
