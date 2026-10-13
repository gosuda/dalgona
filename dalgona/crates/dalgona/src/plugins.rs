// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! Cold-load check that every configured plugin exists under this product's root.

use std::path::PathBuf;

/// A configured plugin with no directory under the product's plugin tree.
#[derive(Debug, thiserror::Error)]
#[error("plugin \"{name}\" is listed in `plugins` but {} does not exist", path.display())]
struct MissingPlugin {
    name: Box<str>,
    path: PathBuf,
}

/// Rejects a `plugins` entry whose directory is absent from the product's own
/// data root, so a plugin installed under another product's root never loads.
/// Names that match an embedded bundled plugin are exempt.
///
/// # Errors
/// Returns a section error naming the first missing plugin directory.
pub(crate) fn require_listed(
    cx: &dalgon::BuildCx<'_>,
    bundled: &[dal_core::PluginSource],
) -> Result<(), dalgon::BuildError> {
    let user_listed = cx
        .config
        .plugins()
        .iter()
        .filter(|name| bundled.iter().all(|source| *source.name != ***name));
    for name in user_listed {
        let path = cx.data_root.join("plugins").join(&**name);
        if !path.is_dir() {
            return Err(dalgon::BuildError::Section {
                section: "plugins".into(),
                source: Box::new(MissingPlugin {
                    name: name.clone(),
                    path,
                }),
            });
        }
    }
    Ok(())
}
