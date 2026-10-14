// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Regenerate the browsable Dalgona manual mirror from the product callback.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

fn main() -> io::Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let output = root.join("../docs");
    let manuals = (dalgona::product().docs)();
    let manual = manuals
        .into_iter()
        .find(|manual| manual.scheme == "dalgona")
        .ok_or_else(|| io::Error::other("dalgona product did not publish its manual"))?;

    if fs::symlink_metadata(&output)?.file_type().is_symlink() {
        return Err(io::Error::other(
            "dalgona manual directory is a symbolic link",
        ));
    }
    let expected: BTreeSet<_> = manual
        .pages
        .iter()
        .map(|(page, _)| format!("{page}.md"))
        .collect();
    let mut actual = BTreeSet::new();
    collect_markdown(&output, &output, &mut actual)?;
    if let Some(extra) = actual.difference(&expected).next() {
        return Err(io::Error::other(format!(
            "unowned file in dalgona manual mirror: {extra}"
        )));
    }
    for (page, _) in &manual.pages {
        if page.is_empty() || page.contains("..") || Path::new(&page).is_absolute() {
            return Err(io::Error::other(format!(
                "invalid Dalgona manual page: {page}"
            )));
        }
        let path = output.join(format!("{page}.md"));
        if !path.starts_with(&output) {
            return Err(io::Error::other(format!(
                "manual page escapes output: {page}"
            )));
        }
    }
    for (page, text) in manual.pages {
        let path = output.join(format!("{page}.md"));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, text)?;
    }
    Ok(())
}

fn collect_markdown(root: &Path, current: &Path, found: &mut BTreeSet<String>) -> io::Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(io::Error::other(format!(
                "symbolic link in Dalgona manual mirror: {}",
                path.display()
            )));
        }
        if file_type.is_dir() {
            collect_markdown(root, &path, found)?;
        } else if file_type.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("md")
        {
            let relative = path
                .strip_prefix(root)
                .map_err(io::Error::other)?
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            found.insert(relative);
        }
    }
    Ok(())
}
