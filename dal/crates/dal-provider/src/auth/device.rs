//! The device-code sign-in, also the fallback for hosts that cannot bind the
//! browser callback.
//!
//! Polling stays here so the authorization flow owns one deadline and one
//! cancellation token, while the browser and device paths share token exchange.

use std::time::Duration;

use dal_core::Family;
use serde::Deserialize;
use tokio::time::{Instant, sleep, sleep_until};
use tokio_util::sync::CancellationToken;

use crate::ProviderError;

use super::oauth::{
    DeviceUrls, LoginProgress, OAuthHttp, decode_json, error_message, post_json, report_progress,
};

/// The device authorization result required by the ordinary token exchange.
///
/// The returned verifier is provided by the device endpoint. It is deliberately
/// not formatted with `Debug` or included in errors.
pub(crate) struct DeviceGrant {
    pub(crate) authorization_code: String,
    pub(crate) code_verifier: String,
}

#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_auth_id: String,
    #[serde(alias = "usercode")]
    user_code: String,
    #[serde(
        default = "default_poll_interval",
        deserialize_with = "deserialize_interval"
    )]
    interval: u64,
}

#[derive(Deserialize)]
struct DevicePollResponse {
    authorization_code: String,
    code_verifier: String,
}

const fn default_poll_interval() -> u64 {
    5
}

fn deserialize_interval<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Interval {
        Number(u64),
        Text(String),
    }

    match Interval::deserialize(deserializer)? {
        Interval::Number(seconds) => Ok(seconds),
        Interval::Text(seconds) => seconds
            .parse()
            .map_err(<D::Error as serde::de::Error>::custom),
    }
}

/// Request and poll a device code until approval, cancellation, or timeout.
///
/// The remote service's 403 and 404 poll responses both mean that approval is
/// pending. Polling waits the server-provided interval between requests and is
/// bounded by `deadline`; cancellation drops the current request or sleep.
pub(crate) async fn run(
    http: &OAuthHttp<'_>,
    family: Family,
    client_id: &'static str,
    urls: &DeviceUrls,
    deadline: Instant,
    progress: &(dyn Fn(LoginProgress) + Send + Sync),
    cancel: &CancellationToken,
) -> Result<DeviceGrant, ProviderError> {
    let start = post_json(
        http,
        family,
        &urls.usercode,
        &DeviceCodeRequest { client_id },
    )
    .await?;
    if !(200..300).contains(&start.status) {
        return Err(ProviderError::DeviceCode {
            status: start.status,
            message: error_message(&start.body, &[]),
        });
    }
    let device = decode_json::<DeviceCodeResponse>(&start.body).ok_or_else(|| {
        ProviderError::DeviceCode {
            status: start.status,
            message: String::from("invalid device-code response"),
        }
    })?;
    let interval = Duration::from_secs(device.interval.max(1));
    report_progress(
        progress,
        LoginProgress::ShowCode {
            url: urls.page.as_str().to_owned(),
            code: device.user_code.clone(),
        },
        cancel,
    )?;

    loop {
        wait_for_next_poll(deadline, interval, cancel).await?;
        let poll = post_json(
            http,
            family,
            &urls.poll,
            &DevicePollRequest {
                device_auth_id: &device.device_auth_id,
                user_code: &device.user_code,
            },
        )
        .await?;
        if matches!(poll.status, 403 | 404) {
            continue;
        }
        if !(200..300).contains(&poll.status) {
            return Err(ProviderError::DeviceCode {
                status: poll.status,
                message: error_message(&poll.body, &[&device.user_code]),
            });
        }
        let grant = decode_json::<DevicePollResponse>(&poll.body).ok_or_else(|| {
            ProviderError::DeviceCode {
                status: poll.status,
                message: String::from("invalid device-token response"),
            }
        })?;
        return Ok(DeviceGrant {
            authorization_code: grant.authorization_code,
            code_verifier: grant.code_verifier,
        });
    }
}

#[derive(serde::Serialize)]
struct DeviceCodeRequest {
    client_id: &'static str,
}

#[derive(serde::Serialize)]
struct DevicePollRequest<'a> {
    device_auth_id: &'a str,
    user_code: &'a str,
}

async fn wait_for_next_poll(
    deadline: Instant,
    interval: Duration,
    cancel: &CancellationToken,
) -> Result<(), ProviderError> {
    if Instant::now() >= deadline {
        return Err(ProviderError::LoginTimeout);
    }
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(ProviderError::LoginCancelled),
        () = sleep_until(deadline) => Err(ProviderError::LoginTimeout),
        () = sleep(interval) => Ok(()),
    }
}

#[cfg(test)]
mod tests;
