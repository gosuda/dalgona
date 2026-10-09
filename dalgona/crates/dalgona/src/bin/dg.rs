//! The `dg` alias of the dalgona binary.
// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

fn main() -> std::process::ExitCode {
    dalgon::run(dalgona::product())
}
