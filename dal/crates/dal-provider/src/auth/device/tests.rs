use super::{DeviceCodeResponse, decode_json, wait_for_next_poll};
use std::time::Duration;

use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

#[test]
fn device_interval_accepts_number_and_string_and_usercode_alias() {
    let numeric = decode_json::<DeviceCodeResponse>(
        br#"{"device_auth_id":"device","user_code":"ABCD","interval":3}"#,
    );
    assert!(numeric.as_ref().is_some_and(|value| value.interval == 3));

    let text = decode_json::<DeviceCodeResponse>(
        br#"{"device_auth_id":"device","usercode":"ABCD","interval":"7"}"#,
    );
    assert!(
        text.as_ref()
            .is_some_and(|value| value.interval == 7 && value.user_code == "ABCD")
    );
    assert!(
        decode_json::<DeviceCodeResponse>(
            br#"{"device_auth_id":"device","user_code":"ABCD","interval":"bad"}"#
        )
        .is_none()
    );
}

#[tokio::test]
async fn pending_poll_delay_obeys_cancel_and_deadline() {
    let cancel = CancellationToken::new();
    cancel.cancel();
    let cancelled = wait_for_next_poll(
        Instant::now() + Duration::from_secs(10),
        Duration::from_secs(60),
        &cancel,
    )
    .await;
    assert!(matches!(
        cancelled,
        Err(crate::ProviderError::LoginCancelled)
    ));

    let expired = wait_for_next_poll(
        Instant::now(),
        Duration::from_secs(1),
        &CancellationToken::new(),
    )
    .await;
    assert!(matches!(expired, Err(crate::ProviderError::LoginTimeout)));
}
