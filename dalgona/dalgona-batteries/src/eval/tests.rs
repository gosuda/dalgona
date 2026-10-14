// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
use super::Stamp;

#[test]
fn a_stamp_is_rfc_3339_utc_with_milliseconds_and_a_sortable_unique_run_id() {
    let stamp = Stamp::now();
    let bytes = stamp.started_at.as_bytes();
    assert_eq!(bytes.len(), 24, "{}", stamp.started_at);
    assert_eq!(
        (bytes[4], bytes[7], bytes[10], bytes[13], bytes[16]),
        (b'-', b'-', b'T', b':', b':')
    );
    assert_eq!((bytes[19], bytes[23]), (b'.', b'Z'));
    let (time, unique) = stamp.run_id.split_once('-').unwrap();
    assert_eq!(time.len(), 16);
    assert!(time.ends_with('Z') && time.as_bytes()[8] == b'T', "{time}");
    assert_eq!(unique.len(), 8);
    assert_ne!(Stamp::now().run_id, stamp.run_id);
}
