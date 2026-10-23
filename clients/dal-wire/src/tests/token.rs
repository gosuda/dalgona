use crate::token::{TokenError, create, load, read_connect_token};

#[test]
fn creation_is_exclusive_and_force_replaces_atomically() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("serve.token");
    let first = create(&path, false).expect("create token");
    let original = std::fs::read(&path).expect("read token file");
    assert_eq!(original, format!("{first}\n").as_bytes());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            std::fs::metadata(&path).expect("read mode").mode() & 0o777,
            0o600
        );
    }
    assert!(is_token(&first));
    assert!(load(&path).expect("load created token").matches(&first));

    assert!(matches!(
        create(&path, false),
        Err(TokenError::AlreadyExists { .. })
    ));
    assert_eq!(
        std::fs::read(&path).expect("read unchanged token"),
        original
    );

    let second = create(&path, true).expect("force token replacement");
    assert!(is_token(&second));
    assert_ne!(first, second);
    assert!(load(&path).expect("load replacement").matches(&second));
    assert!(!load(&path).expect("load replacement").matches(&first));
}

#[test]
fn load_uses_first_line_and_validates_the_token_shape() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("serve.token");
    let token = "dal_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    std::fs::write(&path, format!("{token}\r\nignored second line\n")).expect("write token file");
    set_private_mode(&path);

    assert!(load(&path).expect("load first token line").matches(token));
    std::fs::write(&path, "not-a-token\n").expect("replace with invalid content");
    set_private_mode(&path);
    assert!(matches!(load(&path), Err(TokenError::Invalid { .. })));
}

#[test]
fn empty_and_relative_token_paths_are_rejected() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("serve.token");
    std::fs::write(&path, "\n").expect("write empty token file");
    set_private_mode(&path);
    assert!(matches!(load(&path), Err(TokenError::Empty { .. })));
    assert!(matches!(
        create(std::path::Path::new("relative/serve.token"), false),
        Err(TokenError::RelativePath { .. })
    ));
    assert!(matches!(
        read_connect_token(std::path::Path::new("relative/connect.token")),
        Err(TokenError::RelativePath { .. })
    ));
}

#[test]
fn connect_token_returns_the_trimmed_first_line() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("connect.token");
    let token = "dal_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    std::fs::write(&path, format!(" {token} \r\nignored second line\n"))
        .expect("write connect token file");
    set_private_mode(&path);

    assert!(matches!(load(&path), Err(TokenError::Invalid { .. })));
    assert_eq!(
        read_connect_token(&path).expect("read connect token"),
        token
    );
}

#[cfg(unix)]
#[test]
fn token_loaders_reject_symbolic_links() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().expect("temporary directory");
    let target = directory.path().join("target.token");
    let path = directory.path().join("connect.token");
    std::fs::write(
        &target,
        "dal_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n",
    )
    .expect("write target token");
    set_private_mode(&target);
    symlink(&target, &path).expect("create symbolic link");

    assert!(matches!(
        read_connect_token(&path),
        Err(TokenError::Symlink { .. })
    ));
    assert!(matches!(load(&path), Err(TokenError::Symlink { .. })));
}

#[cfg(unix)]
#[test]
fn connect_token_reader_requires_mode_0600() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("connect.token");
    let token = "dal_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    std::fs::write(&path, format!("{token}\n")).expect("write token file");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400))
        .expect("set owner-only mode");

    assert!(matches!(
        load(&path),
        Err(TokenError::ModeNot0600 { mode: 0o400, .. })
    ));
    assert!(matches!(
        read_connect_token(&path),
        Err(TokenError::ModeNot0600 { mode: 0o400, .. })
    ));
}

#[cfg(unix)]
#[test]
fn group_or_other_read_permission_is_rejected() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("serve.token");
    std::fs::write(
        &path,
        "dal_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n",
    )
    .expect("write token file");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
        .expect("set public mode");
    let mode = std::fs::metadata(&path).expect("read mode").mode() & 0o777;

    assert!(matches!(
        load(&path),
        Err(TokenError::OpenToOtherUsers { mode: observed, .. }) if observed == mode
    ));
    assert!(matches!(
        read_connect_token(&path),
        Err(TokenError::OpenToOtherUsers { mode: observed, .. }) if observed == mode
    ));
}

#[test]
fn debug_output_never_contains_token_bytes() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("serve.token");
    let token = create(&path, false).expect("create token");
    let loaded = load(&path).expect("load created token");
    let rendered = format!("{loaded:?}");
    assert_eq!(rendered, "SecretToken([REDACTED])");
    assert!(!rendered.contains(&token));
}

#[test]
fn matches_rejects_anything_but_the_exact_token() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("serve.token");
    let token = create(&path, false).expect("create token");
    let loaded = load(&path).expect("load created token");
    assert!(loaded.matches(&token));
    assert!(!loaded.matches(""));
    assert!(!loaded.matches("dal_short"));
    let mut wrong = token.clone();
    let last = wrong.pop().expect("token is non-empty");
    wrong.push(if last == 'a' { 'b' } else { 'a' });
    assert!(!loaded.matches(&wrong));
    assert!(!loaded.matches(&token.to_uppercase()));
}

#[test]
fn malformed_first_lines_fail_closed() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let cases = [
        "dal_0123456789ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef\n",
        "dal_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcde\n",
        "dal_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0\n",
        "dal-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n",
        "dal_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdeg\n",
    ];
    for (index, contents) in cases.iter().enumerate() {
        let path = directory.path().join(format!("bad-{index}.token"));
        std::fs::write(&path, contents).expect("write malformed token file");
        set_private_mode(&path);
        assert!(
            matches!(load(&path), Err(TokenError::Invalid { .. })),
            "load accepts case {index}"
        );
        assert!(
            matches!(read_connect_token(&path), Err(TokenError::Invalid { .. })),
            "connect reader accepts case {index}"
        );
    }
}

#[test]
fn missing_and_non_regular_paths_fail_closed() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let missing = directory.path().join("absent.token");
    assert!(matches!(load(&missing), Err(TokenError::Missing { .. })));
    assert!(matches!(
        read_connect_token(&missing),
        Err(TokenError::Missing { .. })
    ));
    assert!(matches!(
        load(directory.path()),
        Err(TokenError::NotRegularFile { .. })
    ));
    assert!(matches!(
        read_connect_token(directory.path()),
        Err(TokenError::NotRegularFile { .. })
    ));
    assert!(matches!(
        load(std::path::Path::new("relative/serve.token")),
        Err(TokenError::RelativePath { .. })
    ));
    assert!(matches!(
        create(std::path::Path::new("relative/serve.token"), true),
        Err(TokenError::RelativePath { .. })
    ));
}

#[test]
fn oversized_first_line_is_invalid_not_truncated_to_valid() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("serve.token");
    std::fs::write(&path, "a".repeat(5000)).expect("write oversized token file");
    set_private_mode(&path);
    assert!(matches!(load(&path), Err(TokenError::Invalid { .. })));
    assert!(matches!(
        read_connect_token(&path),
        Err(TokenError::Invalid { .. })
    ));
}

fn is_token(token: &str) -> bool {
    token.len() == 68
        && token.starts_with("dal_")
        && token.as_bytes()[4..]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

#[cfg(unix)]
fn set_private_mode(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .expect("set private token mode");
}

#[cfg(not(unix))]
fn set_private_mode(_path: &std::path::Path) {}
