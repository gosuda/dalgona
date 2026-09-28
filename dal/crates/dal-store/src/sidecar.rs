//! Checked, session-scoped access to private sidecar files.

use std::{fs, io, path::PathBuf};

use crate::{
    error::StoreError,
    layout::SessionPaths,
    util::{self, FileMode},
};

/// Reads and atomically writes private files within one file-backed session.
#[derive(Debug)]
pub struct Sidecar {
    session_dir: PathBuf,
}

impl Sidecar {
    pub(crate) fn new(paths: &SessionPaths) -> Self {
        Self {
            session_dir: paths.directory().to_path_buf(),
        }
    }

    /// Atomically replaces the named sidecar with `bytes`, using mode 0600.
    ///
    /// # Errors
    /// Returns [`StoreError::Invalid`] when `name` is not a valid sidecar name, or
    /// [`StoreError::Io`] when atomic publication fails.
    pub fn write(&self, name: &str, bytes: &[u8]) -> Result<(), StoreError> {
        let path = self.path(name)?;
        util::write_atomic(&path, bytes, FileMode::Mode0600)
    }

    /// Reads the named sidecar bytes without decoding or interpreting their payload.
    ///
    /// # Errors
    /// Returns [`StoreError::Invalid`] when `name` is not a valid sidecar name,
    /// [`StoreError::NotFound`] with the sidecar path when it is missing, or
    /// [`StoreError::Io`] for another read failure.
    pub fn read(&self, name: &str) -> Result<Vec<u8>, StoreError> {
        let path = self.path(name)?;
        match fs::read(&path) {
            Ok(bytes) => Ok(bytes),
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                Err(StoreError::NotFound { path })
            }
            Err(source) => Err(util::io_err(&path, source)),
        }
    }

    fn path(&self, name: &str) -> Result<PathBuf, StoreError> {
        let bytes = name.as_bytes();
        let valid = !bytes.is_empty()
            && bytes.len() <= 64
            && bytes[0] != b'.'
            && bytes
                .iter()
                .all(|&byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
        if !valid {
            return Err(StoreError::Invalid {
                reason: "sidecar name must be 1 to 64 ASCII alphanumeric, '.', '_', or '-' characters and must not start with '.'".into(),
            });
        }
        Ok(self.session_dir.join(name))
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use dal_core::SessionId;

    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "dal-store-sidecar-{}-{}",
                std::process::id(),
                util::random_hex()
            ));
            fs::DirBuilder::new()
                .create(&path)
                .expect("create sidecar test directory");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn sidecar() -> (TempDir, SessionPaths, Sidecar) {
        let root = TempDir::new();
        let session =
            SessionId::parse("0192aa00-0000-7000-8000-000000000001").expect("valid UUIDv7");
        let paths = SessionPaths::new(&root.0, "workspace-key", session);
        fs::create_dir_all(paths.directory()).expect("create session directory");
        let sidecar = Sidecar::new(&paths);
        (root, paths, sidecar)
    }

    #[test]
    fn reads_and_writes_names_at_grammar_boundaries() {
        let (_root, _paths, sidecar) = sidecar();
        let maximum = "x".repeat(64);

        sidecar
            .write("a", b"one byte name")
            .expect("write shortest name");
        sidecar
            .write("A0._-", b"allowed punctuation")
            .expect("write allowed ASCII punctuation");
        sidecar
            .write(&maximum, b"sixty-four byte name")
            .expect("write maximum-length name");

        assert_eq!(
            sidecar.read("a").expect("read shortest name"),
            b"one byte name"
        );
        assert_eq!(
            sidecar.read("A0._-").expect("read allowed punctuation"),
            b"allowed punctuation"
        );
        assert_eq!(
            sidecar.read(&maximum).expect("read maximum-length name"),
            b"sixty-four byte name"
        );
    }

    #[test]
    fn rejects_names_outside_the_sidecar_grammar_without_path_escape() {
        let (_root, paths, sidecar) = sidecar();
        let too_long = "x".repeat(65);
        let invalid_names = [
            "",
            "a/b",
            "a\\b",
            "café",
            "a\u{7}b",
            ".hidden",
            "../outside",
            "a/../../outside",
            too_long.as_str(),
        ];

        for name in invalid_names {
            assert!(matches!(
                sidecar.write(name, b"no"),
                Err(StoreError::Invalid { .. })
            ));
            assert!(matches!(
                sidecar.read(name),
                Err(StoreError::Invalid { .. })
            ));
        }

        assert!(!paths.directory().join("../outside").exists());
    }

    #[test]
    fn write_replaces_an_existing_sidecar() {
        let (_root, _paths, sidecar) = sidecar();

        sidecar.write("state", b"old").expect("write initial value");
        sidecar.write("state", b"new value").expect("replace value");

        assert_eq!(
            sidecar.read("state").expect("read replacement"),
            b"new value"
        );
    }

    #[test]
    fn missing_read_returns_typed_not_found_with_sidecar_path() {
        let (_root, paths, sidecar) = sidecar();
        let expected_path = paths.directory().join("missing");

        let error = sidecar.read("missing").expect_err("sidecar is absent");

        assert!(matches!(
            error,
            StoreError::NotFound { path } if path == expected_path
        ));
    }

    #[cfg(unix)]
    #[test]
    fn write_uses_private_mode_on_unix() {
        use std::os::unix::fs::PermissionsExt;

        let (_root, paths, sidecar) = sidecar();
        sidecar
            .write("private", b"secret")
            .expect("write private sidecar");

        let mode = fs::metadata(paths.directory().join("private"))
            .expect("stat sidecar")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}
