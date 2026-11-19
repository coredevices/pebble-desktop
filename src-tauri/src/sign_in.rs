use crate::cloud::{Cloud, Error};
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;
use tauri_plugin_opener::OpenerExt;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{watch, Mutex},
};
use url::Url;

const BROKER: &str = "https://appstore-api.repebble.com";
const CLIENT_KEY: &str = "pebble-cli-public";

#[derive(Default)]
pub struct SignIn {
    running: Mutex<()>,
    cancel: Mutex<Option<watch::Sender<bool>>>,
}

#[derive(Deserialize)]
struct Started {
    flow_id: String,
    auth_url: String,
}
#[derive(Deserialize)]
struct Exchange {
    status: String,
    credential_type: Option<String>,
    credential: Option<String>,
}

impl SignIn {
    pub async fn cancel(&self) {
        if let Some(sender) = self.cancel.lock().await.as_ref() {
            let _ = sender.send(true);
        }
    }

    pub async fn start(&self, app: &tauri::AppHandle, cloud: &Cloud) -> Result<String, Error> {
        let _guard = self
            .running
            .try_lock()
            .map_err(|_| Error::new("busy", "Sign-in is already open in your browser."))?;
        let (sender, mut receiver) = watch::channel(false);
        *self.cancel.lock().await = Some(sender);
        let result = tokio::select! {
            result = tokio::time::timeout(Duration::from_secs(300), self.run(app, cloud)) =>
                result.unwrap_or(Err(Error::new("timeout", "Sign-in expired. Please try again."))),
            _ = receiver.changed() => Err(Error::new("cancelled", "Sign-in cancelled.")),
        };
        *self.cancel.lock().await = None;
        result
    }

    async fn run(&self, app: &tauri::AppHandle, cloud: &Cloud) -> Result<String, Error> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(Error::network)?;
        let port = listener.local_addr().map_err(Error::network)?.port();
        let path = format!("/pebble-signin/{}", uuid::Uuid::new_v4());
        let response = cloud
            .client
            .post(format!("{BROKER}/api/v1/cli-oauth/start"))
            .header("x-pebble-client-key", CLIENT_KEY)
            .json(&json!({"callback_url":format!("http://127.0.0.1:{port}{path}")}))
            .send()
            .await
            .map_err(Error::network)?;
        if !response.status().is_success() {
            return Err(Error::new(
                "sign_in",
                "Couldn't start sign-in. Please try again.",
            ));
        }
        let started: Started = response.json().await.map_err(Error::network)?;
        let url = Url::parse(&started.auth_url).map_err(Error::network)?;
        if url.origin().ascii_serialization() != BROKER
            || url.path() != "/api/v1/cli-oauth/authorize"
            || url
                .query_pairs()
                .find(|(key, _)| key == "flow_id")
                .is_none_or(|(_, id)| !flow_matches(&id, &started.flow_id))
        {
            return Err(Error::new(
                "sign_in",
                "Couldn't start sign-in. Please try again.",
            ));
        }
        app.opener()
            .open_url(url.as_str(), None::<&str>)
            .map_err(Error::network)?;
        loop {
            let (mut stream, address) = listener.accept().await.map_err(Error::network)?;
            if !address.ip().is_loopback() {
                continue;
            }
            let request = tokio::time::timeout(Duration::from_secs(3), async {
                let mut bytes = Vec::new();
                let mut buffer = [0_u8; 1024];
                while bytes.len() < 8192 {
                    let count = stream.read(&mut buffer).await?;
                    if count == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buffer[..count]);
                    if bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                Ok::<_, std::io::Error>(bytes)
            })
            .await;
            let Ok(Ok(request)) = request else {
                continue;
            };
            let result = callback(&request, &path, &started.flow_id, port);
            let (status, text) = match result {
                Some(true) => (
                    "200 OK",
                    "Sign-in received. You can return to the Pebble app.",
                ),
                Some(false) => (
                    "200 OK",
                    "Sign-in cancelled. You can return to the Pebble app.",
                ),
                None => ("404 Not Found", "Not found."),
            };
            let message = format!("HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{text}", text.len());
            let _ =
                tokio::time::timeout(Duration::from_secs(2), stream.write_all(message.as_bytes()))
                    .await;
            match result {
                Some(true) => break,
                Some(false) => return Err(Error::new("cancelled", "Sign-in cancelled.")),
                None => continue,
            }
        }
        // No token ever appears in the callback URL; the broker exchanges a one-time flow.
        let response = cloud
            .client
            .post(format!("{BROKER}/api/v1/cli-oauth/exchange"))
            .header("x-pebble-client-key", CLIENT_KEY)
            .json(&json!({"flow_id":started.flow_id}))
            .send()
            .await
            .map_err(Error::network)?;
        if !response.status().is_success() {
            return Err(Error::new(
                "sign_in",
                "Couldn't finish sign-in. Please try again.",
            ));
        }
        let result: Exchange = response.json().await.map_err(Error::network)?;
        if result.status != "ok" || result.credential_type.as_deref() != Some("custom_token") {
            return Err(Error::new(
                "sign_in",
                "Couldn't finish sign-in. Please try again.",
            ));
        }
        let token = result.credential.ok_or(Error::new(
            "sign_in",
            "Couldn't finish sign-in. Please try again.",
        ))?;
        // A fresh Firebase login is not necessarily an existing Pebble account.
        cloud.validate_custom_token(&token).await?;
        Ok(token)
    }
}

fn callback(bytes: &[u8], path: &str, flow: &str, port: u16) -> Option<bool> {
    let request = std::str::from_utf8(bytes).ok()?;
    let mut lines = request.split("\r\n");
    let mut first = lines.next()?.split_whitespace();
    if first.next()? != "GET" {
        return None;
    }
    let target = first.next()?;
    if !target.starts_with('/') || target.starts_with("//") {
        return None;
    }
    let host = lines.find_map(|line| {
        line.split_once(':')
            .filter(|(k, _)| k.eq_ignore_ascii_case("host"))
            .map(|(_, v)| v.trim())
    });
    if host != Some(format!("127.0.0.1:{port}").as_str()) {
        return None;
    }
    let url = Url::parse(&format!("http://127.0.0.1:{port}{target}")).ok()?;
    if url.path() != path {
        return None;
    }
    let query: Vec<_> = url.query_pairs().collect();
    // The random path binds the callback to this sign-in; the broker no longer
    // always echoes flow_id, but one that is present must match.
    if query.iter().filter(|(k, _)| k == "flow_id").count() > 1
        || query.iter().filter(|(k, _)| k == "broker_status").count() != 1
        || query.iter().any(|(k, v)| k == "flow_id" && !flow_matches(v, flow))
    {
        return None;
    }
    query
        .iter()
        .find(|(key, _)| key == "broker_status")
        .map(|(_, value)| value == "ok")
}

/// The broker's flow_id is "<id>.<secret>"; URLs it builds carry only "<id>".
fn flow_matches(value: &str, flow: &str) -> bool {
    value == flow || flow.split_once('.').is_some_and(|(id, _)| id == value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn callback_is_bound_to_random_path_flow_and_local_host() {
        let valid =
            "GET /private?broker_status=ok&flow_id=flow HTTP/1.1\r\nHost: 127.0.0.1:1234\r\n\r\n";
        assert_eq!(
            callback(valid.as_bytes(), "/private", "flow", 1234),
            Some(true)
        );
        for bad in [
            valid.replace("flow_id=flow", "flow_id=wrong"),
            valid.replace("/private", "/other"),
            valid.replace("127.0.0.1:1234", "evil.example"),
            valid.replace("GET", "POST"),
            valid.replace("flow_id=flow", "flow_id=flow&flow_id=other"),
        ] {
            assert_eq!(callback(bad.as_bytes(), "/private", "flow", 1234), None);
        }
        // Broker flow ids are "<id>.<secret>"; callbacks may carry just "<id>".
        assert_eq!(callback(valid.as_bytes(), "/private", "flow.secret", 1234), Some(true));
        assert!(!flow_matches("flow.secret", "flow"));
        // Current broker callback: no flow_id at all.
        let bare = valid.replace("&flow_id=flow", "");
        assert_eq!(callback(bare.as_bytes(), "/private", "flow.secret", 1234), Some(true));
        assert!(!flow_matches("", "flow.secret"));
    }
}
