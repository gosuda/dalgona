// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Durable goal state machine: sidecar codec, goal tools and commands,
//! continuation verdict, read-only goal projection, and P4 prompt
//! construction. The verdict stays pure; persistence flows only through the
//! core's `sidecar` service.

pub(crate) mod adapter;
pub(crate) mod ops;
pub(crate) mod policy;
pub(crate) mod prompt;
pub(crate) mod sidecar;

#[cfg(test)]
mod tests;
