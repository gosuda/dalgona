// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
use std::path::PathBuf;

use super::{Origin, PACK_SUFFIX, ResolveError, resolve};

fn root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wf-eval-resolve-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const USER: &str = "evalpack.pack(name = \"edit-anchor-smoke\", cases = [])\n";

#[test]
fn a_named_pack_resolves_from_the_user_root() {
    let dir = root("named");
    let path = dir.join(format!("mine{PACK_SUFFIX}"));
    std::fs::write(&path, USER).unwrap();
    let found = resolve("mine", &dir).unwrap();
    assert_eq!(found.name, "mine");
    assert_eq!(found.source, USER);
    assert_eq!(found.origin, Origin::User { path });
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn an_explicit_file_resolves_by_path_and_takes_its_name_from_the_file() {
    let dir = root("path");
    let path = dir.join(format!("elsewhere{PACK_SUFFIX}"));
    std::fs::write(&path, USER).unwrap();
    let found = resolve(path.to_str().unwrap(), &dir).unwrap();
    assert_eq!(found.name, "elsewhere");
    let missing = resolve(dir.join("nope.eval.star").to_str().unwrap(), &dir).unwrap_err();
    assert!(matches!(missing, ResolveError::Read { .. }), "{missing}");
    let wrong = resolve(dir.join("plain.star").to_str().unwrap(), &dir).unwrap_err();
    assert!(matches!(wrong, ResolveError::NotAPack { .. }), "{wrong}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn names_that_could_escape_the_pack_root_are_refused() {
    let dir = root("names");
    for name in ["", "-x", "a b", "a.b", "é", &"a".repeat(65)] {
        assert!(
            matches!(resolve(name, &dir), Err(ResolveError::BadName)),
            "{name:?} was accepted"
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn an_unknown_name_names_the_user_directory() {
    let dir = root("unknown");
    let text = resolve("nothing", &dir).unwrap_err().to_string();
    assert!(text.contains("no eval pack"), "{text}");
    assert!(text.contains(dir.to_str().unwrap()), "{text}");
    std::fs::remove_dir_all(dir).unwrap();
}
