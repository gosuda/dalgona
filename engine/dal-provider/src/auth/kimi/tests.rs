use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

use super::{
    CLIENT_PLATFORM, ClientIdentity, ClientIdentitySource, DEVICE_ID_FILE, USER_AGENT_PRODUCT,
    device_id, installed_device_id,
};
use crate::{EnvSnapshot, ProviderIdentity};
#[cfg(unix)]
use std::path::Path;

fn identity() -> ProviderIdentity {
    ProviderIdentity {
        version: Box::from("1.2.3"),
        os: Box::from("linux"),
        os_version: Box::from("6.1"),
        arch: Box::from("x86_64"),
    }
}

/// A private per-test directory under the system temporary directory,
/// removed on drop.
struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "dal-provider-kimi-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create test directory");
        Self(path)
    }

    fn write_device_id(&self, value: &str) {
        fs::write(self.0.join(DEVICE_ID_FILE), format!("{value}\n"))
            .expect("write the device-id file");
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn headers_of(identity: &ClientIdentity) -> Vec<(&'static str, String)> {
    identity.headers().to_vec()
}

fn header<'a>(headers: &'a [(&'static str, String)], name: &str) -> &'a str {
    headers
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map_or_else(
            || panic!("the identity carries {name}"),
            |(_, value)| value.as_str(),
        )
}

#[test]
fn the_identity_carries_the_pinned_client_set() {
    let env = EnvSnapshot::test(&[("HOSTNAME", "dev-box")]);
    let identity = ClientIdentity::new(&identity(), &env, "device-1234");
    let headers = headers_of(&identity);

    assert_eq!(headers.len(), 6, "one pinned set, no extras: {headers:?}");
    assert_eq!(header(&headers, "x-msh-platform"), CLIENT_PLATFORM);
    assert_eq!(header(&headers, "x-msh-version"), "1.2.3");
    assert_eq!(header(&headers, "x-msh-device-id"), "device-1234");
    assert_eq!(header(&headers, "x-msh-device-name"), "dev-box");
    assert_eq!(header(&headers, "x-msh-device-model"), "Linux 6.1 x86_64");
    assert_eq!(header(&headers, "x-msh-os-version"), "6.1");
}

#[test]
fn the_user_agent_names_the_official_product_and_our_version() {
    let env = EnvSnapshot::test(&[("HOSTNAME", "dev-box")]);
    let identity = ClientIdentity::new(&identity(), &env, "device-1234");

    assert_eq!(USER_AGENT_PRODUCT, "kimi-code-cli");
    assert_eq!(identity.user_agent(), "kimi-code-cli/1.2.3");
}

#[test]
fn identity_values_drop_everything_outside_printable_ascii() {
    let env = EnvSnapshot::test(&[("HOSTNAME", " hóst-box\n")]);
    let identity = ClientIdentity::new(&identity(), &env, "dévice");
    let headers = headers_of(&identity);

    assert_eq!(header(&headers, "x-msh-device-name"), "hst-box");
    assert_eq!(header(&headers, "x-msh-device-id"), "dvice");
}

#[test]
fn an_empty_source_falls_back_to_unknown() {
    let env = EnvSnapshot::test(&[]);
    let identity = ClientIdentity::new(&identity(), &env, "device-1234");

    assert_eq!(
        header(&headers_of(&identity), "x-msh-device-name"),
        "unknown"
    );
}

#[test]
fn the_host_name_falls_back_through_the_captured_variables() {
    let renamed = EnvSnapshot::test(&[("COMPUTERNAME", "win-box")]);
    let client = ClientIdentity::new(&identity(), &renamed, "device-1234");
    assert_eq!(header(&headers_of(&client), "x-msh-device-name"), "win-box");

    // `HOSTNAME` wins when both are captured, in the pinned order.
    let both = EnvSnapshot::test(&[("COMPUTERNAME", "win-box"), ("HOSTNAME", "unix-box")]);
    let combined = ClientIdentity::new(&identity(), &both, "device-1234");
    assert_eq!(
        header(&headers_of(&combined), "x-msh-device-name"),
        "unix-box"
    );
}

#[test]
fn a_blank_host_name_is_unknown_not_empty() {
    let env = EnvSnapshot::test(&[("HOSTNAME", "   ")]);
    let identity = ClientIdentity::new(&identity(), &env, "device-1234");

    assert_eq!(
        header(&headers_of(&identity), "x-msh-device-name"),
        "unknown"
    );
}

#[cfg(unix)]
fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).expect("stat").permissions().mode()
}

#[test]
fn the_sign_in_publishes_one_stable_owner_only_device_id() {
    let dir = TestDir::new("publish");

    let first = device_id(&dir.0);
    assert_eq!(first.len(), 32, "a fresh id is 32 hex characters: {first}");
    let written = dir.0.join(DEVICE_ID_FILE);
    assert_eq!(fs::read_to_string(&written).unwrap().trim(), first);
    #[cfg(unix)]
    assert_eq!(mode_of(&written) & 0o777, 0o600, "owner-only device id");

    // The second sign-in of the same install reuses the published id.
    assert_eq!(device_id(&dir.0), first);
}

#[test]
fn the_sign_in_never_replaces_a_published_device_id() {
    let dir = TestDir::new("keep");
    dir.write_device_id("published-id");

    assert_eq!(device_id(&dir.0), "published-id");
    assert_eq!(
        fs::read_to_string(dir.0.join(DEVICE_ID_FILE))
            .unwrap()
            .trim(),
        "published-id"
    );
}

#[test]
fn requests_read_the_published_id_and_write_nothing() {
    let dir = TestDir::new("installed");
    dir.write_device_id("published-id");
    assert_eq!(installed_device_id(&dir.0), "published-id");

    // With no sign-in on record the id is process-local, and no file appears.
    let absent = TestDir::new("absent");
    let local = installed_device_id(&absent.0);
    assert_ne!(local, "");
    assert!(!absent.0.join(DEVICE_ID_FILE).exists());
}

#[tokio::test]
async fn the_set_source_derives_one_identity_for_every_caller() {
    let dir = TestDir::new("source");
    dir.write_device_id("published-id");
    let source = ClientIdentitySource::new(identity(), EnvSnapshot::test(&[]), dir.0.clone());

    let first = source.get().await.expect("the derivation succeeds");
    let second = source.get().await.expect("the derivation succeeds");
    assert!(Arc::ptr_eq(&first, &second), "one derivation per source");
    assert_eq!(
        header(&headers_of(&first), "x-msh-device-id"),
        "published-id"
    );
    assert_eq!(
        first.headers().first().map(|entry| entry.0),
        Some("X-Msh-Platform")
    );
}
