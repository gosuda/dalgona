//! Probe: can another crate construct a journal record?

use std::num::NonZeroU64;

use dal_core::{Gen, Header, Product, Record, SessionId, Workspace};

/// Builds one boot record. Compile failure here means the vocabulary is closed.
pub fn probe() -> Record {
    let workspace = Workspace::new(std::path::PathBuf::from("/tmp/probe")).expect("absolute");
    Record::Boot {
        at: jiff::Timestamp::UNIX_EPOCH,
        r#gen: Gen::new(NonZeroU64::MIN),
        version: "0.1.0".into(),
    }
}

#[allow(dead_code)]
fn header_probe() -> Header {
    Header {
        id: SessionId::new_v7(),
        at: jiff::Timestamp::UNIX_EPOCH,
        workspace: Workspace::new(std::path::PathBuf::from("/ws")).expect("absolute"),
        product: Product::Dal,
        from: None,
    }
}
