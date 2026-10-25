//! Remote notifications and macOS Keychain storage.
//!
//! Credentials are never formatted with `Debug`, written to application files,
//! included in URLs sent to a provider, or copied into returned error messages.

use std::time::Duration;

use reqwest::{Client, RequestBuilder, Response, StatusCode, Url};
use serde::{Deserialize, Serialize};

const KEYCHAIN_ACCOUNT: &str = "notification-credentials-v1";
const MAX_RESPONSE_BYTES: usize = 16 * 1024;
const PUSHOVER_ENDPOINT: &str = "https://api.pushover.net/1/messages.json";

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Credentials {
    pub pushover_token: String,
    pub pushover_user: String,
    pub bark_url: String,
}

impl Credentials {
    /// Validate only supplied values; users can configure a provider in stages.
    /// Sending additionally requires all credentials for that provider.
    pub fn validate(&self) -> Result<(), String> {
        for (value, label) in [
            (&self.pushover_token, "Pushover application token"),
            (&self.pushover_user, "Pushover user key"),
        ] {
            if !value.is_empty()
                && (value.len() != 30 || !value.bytes().all(|b| b.is_ascii_alphanumeric()))
            {
                return Err(format!("{label} must contain 30 letters or numbers."));
            }
        }
        if !self.bark_url.is_empty() {
            bark_endpoint(&self.bark_url)?;
        }
        Ok(())
    }
}

/// Call Keychain operations on a blocking thread from async request handlers:
/// macOS may ask the user to allow access to an existing Keychain item.
#[derive(Clone)]
pub struct CredentialStore {
    namespace: String,
}

impl CredentialStore {
    pub fn new(namespace: &str) -> Self {
        Self {
            namespace: namespace.to_owned(),
        }
    }

    #[cfg(target_os = "macos")]
    pub fn load(&self) -> Result<Credentials, String> {
        use security_framework::passwords::{PasswordOptions, generic_password};

        let options = PasswordOptions::new_generic_password(&self.namespace, KEYCHAIN_ACCOUNT);
        match generic_password(options) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| {
                "The saved notification credentials could not be read. Re-enter them in Settings."
                    .to_owned()
            }),
            // errSecItemNotFound: no configuration has been saved yet.
            Err(error) if error.code() == -25300 => Ok(Credentials::default()),
            Err(error) => Err(format!(
                "Could not read notification credentials from Keychain (macOS error {}).",
                error.code()
            )),
        }
    }

    #[cfg(target_os = "macos")]
    pub fn save(&self, credentials: &Credentials) -> Result<(), String> {
        credentials.validate()?;
        let bytes = serde_json::to_vec(credentials)
            .map_err(|_| "Could not encode notification credentials.".to_owned())?;
        security_framework::passwords::set_generic_password(
            &self.namespace,
            KEYCHAIN_ACCOUNT,
            &bytes,
        )
        .map_err(|error| {
            format!(
                "Could not save notification credentials to Keychain (macOS error {}).",
                error.code()
            )
        })
    }

    #[cfg(not(target_os = "macos"))]
    pub fn load(&self) -> Result<Credentials, String> {
        Err("Notification credential storage currently requires macOS Keychain.".to_owned())
    }

    #[cfg(not(target_os = "macos"))]
    pub fn save(&self, _credentials: &Credentials) -> Result<(), String> {
        Err("Notification credential storage currently requires macOS Keychain.".to_owned())
    }
}

#[derive(Clone)]
pub struct Notifier {
    http: Client,
}

impl Notifier {
    pub fn new() -> Result<Self, String> {
        let http = Client::builder()
            .https_only(true)
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(12))
            // Never forward a POST containing credentials through a redirect.
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("ApplePickupMonitor/0.1")
            .build()
            .map_err(|_| "Could not initialize the notification HTTP client.".to_owned())?;
        Ok(Self { http })
    }

    pub async fn send(
        &self,
        channel: &str,
        credentials: &Credentials,
        title: &str,
        message: &str,
        url: Option<&str>,
    ) -> Result<(), String> {
        let request = self.request(channel, credentials, title, message, url)?;
        let response = request.send().await.map_err(|error| {
            // reqwest errors can contain the URL. Even a self-hosted server's
            // hostname/path may be sensitive, so do not format the error.
            if error.is_timeout() {
                format!(
                    "{} request timed out; delivery could not be confirmed.",
                    label(channel)
                )
            } else {
                format!(
                    "Could not connect to {}. Check your network and provider settings.",
                    label(channel)
                )
            }
        })?;
        finish_response(channel, response).await
    }

    fn request(
        &self,
        channel: &str,
        credentials: &Credentials,
        title: &str,
        message: &str,
        url: Option<&str>,
    ) -> Result<RequestBuilder, String> {
        if message.trim().is_empty() {
            return Err("Notification message must not be empty.".to_owned());
        }
        let url = notification_url(url)?;
        let title = truncate_chars(title, 250);
        let message = truncate_chars(message, 1024);
        match channel {
            "pushover" => {
                if credentials.pushover_token.is_empty() || credentials.pushover_user.is_empty() {
                    return Err(
                        "Enter both your Pushover application token and user key first.".to_owned(),
                    );
                }
                // A problem in the other provider must not block this one.
                Credentials {
                    pushover_token: credentials.pushover_token.clone(),
                    pushover_user: credentials.pushover_user.clone(),
                    ..Default::default()
                }
                .validate()?;
                let mut body = serde_json::json!({
                    "token": credentials.pushover_token,
                    "user": credentials.pushover_user,
                    "title": title,
                    "message": message,
                    "priority": 0,
                });
                if let Some(url) = url {
                    body["url"] = url.into();
                    body["url_title"] = "View on Apple".into();
                }
                Ok(self.http.post(PUSHOVER_ENDPOINT).json(&body))
            }
            "bark" => {
                let (endpoint, device_key) = bark_endpoint(&credentials.bark_url)?;
                let mut body = serde_json::json!({
                    "device_key": device_key,
                    "title": title,
                    "body": message,
                    "group": "Apple Pickup Monitor",
                });
                if let Some(url) = url {
                    body["url"] = url.into();
                }
                Ok(self.http.post(endpoint).json(&body))
            }
            _ => Err("Unknown notification channel.".to_owned()),
        }
    }
}

fn label(channel: &str) -> &'static str {
    match channel {
        "pushover" => "Pushover",
        "bark" => "Bark",
        _ => "notification provider",
    }
}

fn truncate_chars(value: &str, max: usize) -> &str {
    value
        .char_indices()
        .nth(max)
        .map_or(value, |(end, _)| &value[..end])
}

fn notification_url(url: Option<&str>) -> Result<Option<&str>, String> {
    let Some(url) = url.filter(|url| !url.is_empty()) else {
        return Ok(None);
    };
    if url.len() > 512 {
        return Err("Notification link exceeds the provider's 512-character limit.".to_owned());
    }
    let parsed =
        Url::parse(url).map_err(|_| "Notification link is not a valid web address.".to_owned())?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(
            "Notification link must be an HTTP or HTTPS web address without credentials."
                .to_owned(),
        );
    }
    Ok(Some(url))
}

/// Convert a copied Bark device URL to its V2 endpoint. Preserve reverse-proxy
/// path prefixes: `https://host/bark/key` becomes `https://host/bark/push`.
fn bark_endpoint(raw: &str) -> Result<(Url, String), String> {
    const INVALID: &str = "Enter a complete HTTPS Bark URL ending in your device key.";
    let mut url = Url::parse(raw.trim()).map_err(|_| INVALID.to_owned())?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(INVALID.to_owned());
    }
    let key = url
        .path_segments()
        .and_then(|mut segments| segments.rfind(|part| !part.is_empty()))
        .filter(|key| {
            !key.is_empty()
                && key.len() <= 256
                && key
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
        .ok_or_else(|| INVALID.to_owned())?
        .to_owned();
    // Normalize all trailing slashes before removing the key segment.
    let path = url.path().trim_end_matches('/').to_owned();
    url.set_path(&path);
    url.path_segments_mut()
        .map_err(|_| INVALID.to_owned())?
        .pop()
        .push("push");
    Ok((url, key))
}

async fn finish_response(channel: &str, mut response: Response) -> Result<(), String> {
    let status = response.status();
    // HTTP failures need no body. Providers sometimes echo credentials in their
    // diagnostics; never return or log the response text.
    if status != StatusCode::OK {
        return Err(http_error(channel, status));
    }
    if response
        .content_length()
        .is_some_and(|len| len > MAX_RESPONSE_BYTES as u64)
    {
        return Err(format!(
            "{} returned an unexpectedly large response.",
            label(channel)
        ));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| format!("Could not read {}'s delivery confirmation.", label(channel)))?
    {
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(format!(
                "{} returned an unexpectedly large response.",
                label(channel)
            ));
        }
        body.extend_from_slice(&chunk);
    }
    validate_response(channel, &body)
}

fn http_error(channel: &str, status: StatusCode) -> String {
    let name = label(channel);
    match status.as_u16() {
        400 | 401 | 403 => {
            format!("{name} rejected the notification (HTTP {status}). Check the credentials.")
        }
        429 => format!("{name} has rate-limited notifications. Wait before retrying."),
        300..=399 => {
            format!("{name} redirected the request. Use the provider's final HTTPS address.")
        }
        _ => format!("{name} could not confirm delivery (HTTP {status})."),
    }
}

fn validate_response(channel: &str, body: &[u8]) -> Result<(), String> {
    let response: serde_json::Value = serde_json::from_slice(body).map_err(|_| {
        format!(
            "{} returned an invalid delivery confirmation.",
            label(channel)
        )
    })?;
    let accepted = match channel {
        "pushover" => response.get("status").and_then(|v| v.as_i64()) == Some(1),
        "bark" => response.get("code").and_then(|v| v.as_i64()) == Some(200),
        _ => false,
    };
    if accepted {
        Ok(())
    } else {
        Err(format!(
            "{} rejected the notification. Check the credentials and provider account.",
            label(channel)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credentials() -> Credentials {
        Credentials {
            pushover_token: "a".repeat(30),
            pushover_user: "b".repeat(30),
            bark_url: "https://push.example.test/bark/secretKey".to_owned(),
        }
    }

    #[test]
    fn bark_preserves_reverse_proxy_prefix_and_keeps_key_out_of_endpoint() {
        let (endpoint, key) = bark_endpoint("https://push.example.test/bark/secretKey/").unwrap();
        assert_eq!(endpoint.as_str(), "https://push.example.test/bark/push");
        assert_eq!(key, "secretKey");
        let (endpoint, _) = bark_endpoint("https://api.day.app/secretKey").unwrap();
        assert_eq!(endpoint.as_str(), "https://api.day.app/push");
    }

    #[test]
    fn bark_rejects_insecure_or_ambiguous_urls_without_echoing_credentials() {
        for raw in [
            "http://example.test/secretKey",
            "https://user:secret@example.test/secretKey",
            "https://example.test/secretKey?token=secret",
            "https://example.test/secretKey#secret",
            "https://example.test/",
            "https://example.test/secret%2FKey",
            "not a URL with a secret",
        ] {
            let error = bark_endpoint(raw).unwrap_err();
            assert!(!error.contains("secret"));
        }
    }

    #[test]
    fn provider_payloads_include_credentials_only_in_the_post_body() {
        let notifier = Notifier::new().unwrap();
        let creds = credentials();
        for channel in ["pushover", "bark"] {
            let request = notifier
                .request(
                    channel,
                    &creds,
                    "Stock available",
                    "Sydney has stock",
                    Some("https://www.apple.com/au/shop/bag"),
                )
                .unwrap()
                .build()
                .unwrap();
            assert_eq!(request.method(), reqwest::Method::POST);
            assert!(!request.url().as_str().contains("secretKey"));
            assert!(!request.url().as_str().contains(&creds.pushover_token));
            let body: serde_json::Value =
                serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
            if channel == "pushover" {
                assert_eq!(body["token"], creds.pushover_token);
                assert_eq!(body["user"], creds.pushover_user);
                assert_eq!(body["priority"], 0);
            } else {
                assert_eq!(body["device_key"], "secretKey");
            }
        }
    }

    #[test]
    fn only_explicit_provider_success_is_accepted_and_errors_are_redacted() {
        assert!(validate_response("pushover", br#"{"status":1}"#).is_ok());
        assert!(validate_response("bark", br#"{"code":200}"#).is_ok());
        for channel in ["pushover", "bark"] {
            for body in [
                br#"{"status":0,"code":400,"errors":["secretKey"]}"#.as_slice(),
                br#"{"message":"secretKey"}"#.as_slice(),
                b"<html>secretKey</html>".as_slice(),
            ] {
                assert!(
                    !validate_response(channel, body)
                        .unwrap_err()
                        .contains("secretKey")
                );
            }
        }
    }

    #[test]
    fn one_invalid_provider_does_not_prevent_using_the_other() {
        let notifier = Notifier::new().unwrap();
        let mut creds = credentials();
        creds.bark_url = "not a URL".into();
        assert!(
            notifier
                .request("pushover", &creds, "Title", "Message", None)
                .is_ok()
        );
        creds = credentials();
        creds.pushover_token = "incomplete".into();
        assert!(
            notifier
                .request("bark", &creds, "Title", "Message", None)
                .is_ok()
        );
    }

    #[test]
    fn unicode_messages_are_shortened_on_character_boundaries() {
        let notifier = Notifier::new().unwrap();
        let request = notifier
            .request(
                "pushover",
                &credentials(),
                &"🍎".repeat(300),
                &"🍎".repeat(1500),
                None,
            )
            .unwrap()
            .build()
            .unwrap();
        let body: serde_json::Value =
            serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
        assert_eq!(body["title"].as_str().unwrap().chars().count(), 250);
        assert_eq!(body["message"].as_str().unwrap().chars().count(), 1024);
    }

    #[tokio::test]
    async fn oversized_delivery_confirmation_is_rejected() {
        use std::io::{Read, Write};
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = server.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let (mut socket, _) = server.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = [0u8; 4096];
            let _ = socket.read(&mut request).unwrap();
            // Content-Length is intentionally absent; chunked bodies must also
            // obey the cap, rather than trusting a declared length alone.
            let body = vec![b' '; MAX_RESPONSE_BYTES + 1];
            let _ = write!(
                socket,
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n",
                body.len()
            );
            let _ = socket.write_all(&body);
            let _ = socket.write_all(b"\r\n0\r\n\r\n");
        });
        let response = Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap()
            .get(format!("http://{address}/test"))
            .send()
            .await
            .unwrap();
        assert!(
            finish_response("pushover", response)
                .await
                .unwrap_err()
                .contains("large response")
        );
        worker.join().unwrap();
    }
}
