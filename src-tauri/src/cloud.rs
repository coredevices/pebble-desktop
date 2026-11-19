use reqwest::{Client, Response, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;

pub const PROJECT: &str = "coreapp-ce061";
// Public Firebase client configuration, also used by index-webapp.
const API_KEY: &str = "AIzaSyBZ9Cdvwwv9At2lPmc8TxyyEqSXGXejGvc";
const IDENTITY: &str = "https://identitytoolkit.googleapis.com/v1";
const FIRESTORE: &str = "https://firestore.googleapis.com/v1";

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Error {
    pub code: &'static str,
    pub message: &'static str,
}

impl Error {
    pub const fn new(code: &'static str, message: &'static str) -> Self {
        Self { code, message }
    }
    pub fn network(_: impl std::fmt::Display) -> Self {
        Self::new(
            "network",
            "Couldn't connect. Check your connection and try again.",
        )
    }
    pub fn local(_: impl std::fmt::Display) -> Self {
        Self::new(
            "local",
            "Couldn't save this Mac's settings. Please try again.",
        )
    }
}

#[derive(Clone)]
pub struct Cloud {
    pub client: Client,
    identity: String,
    firestore: String,
}

#[derive(Deserialize)]
struct Lookup {
    users: Vec<Identity>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Identity {
    local_id: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedKey {
    pub computer_name: String,
    pub saved_at: String,
}

fn document_root() -> String {
    format!("projects/{PROJECT}/databases/(default)/documents")
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id != "."
        && id != ".."
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl Cloud {
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(25))
                .connect_timeout(Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("HTTP client"),
            identity: IDENTITY.into(),
            firestore: FIRESTORE.into(),
        }
    }

    async fn response(response: Response) -> Result<Response, Error> {
        match response.status() {
            s if s.is_success() => Ok(response),
            StatusCode::UNAUTHORIZED => Err(Error::new("signed_out", "Please sign in again.")),
            StatusCode::FORBIDDEN => Err(Error::new(
                "permission",
                "Your account couldn't access iMessage setup. Please try again later.",
            )),
            _ => Err(Error::new(
                "cloud",
                "Couldn't save iMessage setup. Please try again.",
            )),
        }
    }

    /// Firebase verifies the ID token. Never trust a UID supplied by JavaScript alone.
    pub async fn account(
        &self,
        id_token: &str,
        expected_uid: Option<&str>,
    ) -> Result<String, Error> {
        if id_token.is_empty() || id_token.len() > 16384 {
            return Err(Error::new("signed_out", "Please sign in again."));
        }
        let response = self
            .client
            .post(format!("{}/accounts:lookup?key={API_KEY}", self.identity))
            .json(&json!({"idToken": id_token}))
            .send()
            .await
            .map_err(Error::network)?;
        if response.status() == StatusCode::BAD_REQUEST {
            return Err(Error::new("signed_out", "Please sign in again."));
        }
        let lookup: Lookup = Self::response(response)
            .await?
            .json()
            .await
            .map_err(Error::network)?;
        let uid = lookup
            .users
            .into_iter()
            .next()
            .map(|u| u.local_id)
            .filter(|uid| valid_id(uid) && expected_uid.is_none_or(|expected| expected == uid))
            .ok_or(Error::new(
                "account_changed",
                "Your account changed. Please sign in again.",
            ))?;
        let response = self
            .client
            .get(format!(
                "{}/{}/users/{uid}",
                self.firestore,
                document_root()
            ))
            .query(&[("mask.fieldPaths", "pebble_user_token")])
            .bearer_auth(id_token)
            .send()
            .await
            .map_err(Error::network)?;
        if response.status() == StatusCode::NOT_FOUND {
            return Err(Error::new("no_pebble_account", "No Pebble account exists for this sign-in. Do you want to try a different sign-in?"));
        }
        Self::response(response).await?;
        Ok(uid)
    }

    pub async fn validate_custom_token(&self, custom_token: &str) -> Result<(), Error> {
        let response = self
            .client
            .post(format!(
                "{}/accounts:signInWithCustomToken?key={API_KEY}",
                self.identity
            ))
            .json(&json!({"token":custom_token,"returnSecureToken":true}))
            .send()
            .await
            .map_err(Error::network)?;
        let body: Value = Self::response(response)
            .await?
            .json()
            .await
            .map_err(Error::network)?;
        let token = body["idToken"]
            .as_str()
            .ok_or(Error::new("signed_out", "Please sign in again."))?;
        self.account(token, None).await?;
        Ok(())
    }

    fn key_path(uid: &str, installation: &str) -> String {
        format!(
            "{}/chat_activation_keys/{uid}/keys/{installation}",
            document_root()
        )
    }

    pub async fn metadata(
        &self,
        token: &str,
        uid: &str,
        installation: &str,
    ) -> Result<Option<SavedKey>, Error> {
        let response = self
            .client
            .get(format!(
                "{}/{}",
                self.firestore,
                Self::key_path(uid, installation)
            ))
            .query(&[
                ("mask.fieldPaths", "computerName"),
                ("mask.fieldPaths", "createdAt"),
            ])
            .bearer_auth(token)
            .send()
            .await
            .map_err(Error::network)?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let body: Value = Self::response(response)
            .await?
            .json()
            .await
            .map_err(Error::network)?;
        let Some(name) = body["fields"]["computerName"]["stringValue"].as_str() else {
            return Ok(None);
        };
        let Some(time) = body["fields"]["createdAt"]["timestampValue"].as_str() else {
            return Ok(None);
        };
        Ok(Some(SavedKey {
            computer_name: name.into(),
            saved_at: time.into(),
        }))
    }

    pub async fn upload(
        &self,
        token: &str,
        uid: &str,
        installation: &str,
        name: &str,
        key: &str,
    ) -> Result<SavedKey, Error> {
        let body = upload_body(uid, installation, name, key);
        let response = self
            .client
            .post(format!("{}/{}:commit", self.firestore, document_root()))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .map_err(Error::network)?;
        let body: Value = Self::response(response)
            .await?
            .json()
            .await
            .map_err(Error::network)?;
        let time = body["commitTime"].as_str().ok_or(Error::new(
            "cloud",
            "Couldn't confirm iMessage setup was saved. Please try again.",
        ))?;
        Ok(SavedKey {
            computer_name: name.into(),
            saved_at: time.into(),
        })
    }

    pub async fn remove(&self, token: &str, uid: &str, installation: &str) -> Result<(), Error> {
        let response = self
            .client
            .delete(format!(
                "{}/{}",
                self.firestore,
                Self::key_path(uid, installation)
            ))
            .bearer_auth(token)
            .send()
            .await
            .map_err(Error::network)?;
        if response.status() != StatusCode::NOT_FOUND {
            Self::response(response).await?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn for_test(base: &str) -> Self {
        let mut cloud = Self::new();
        cloud.identity = base.into();
        cloud.firestore = base.into();
        cloud
    }
}

fn upload_body(uid: &str, installation: &str, name: &str, key: &str) -> Value {
    json!({"writes":[{
        "update": {
            "name": Cloud::key_path(uid, installation),
            "fields": {
                "computerName":{"stringValue":name},
                "macValidationData":{"stringValue":key},
                "formatVersion":{"integerValue":"1"},
                "platform":{"stringValue":"macOS"}
            }
        },
        "updateTransforms":[{"fieldPath":"createdAt","setToServerValue":"REQUEST_TIME"}]
    }]})
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    // Real HTTP boundary with synthetic data only. No live Firebase credentials.
    pub async fn serve(
        responses: Vec<(&'static str, u16, Value)>,
    ) -> (Cloud, tokio::task::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let cloud = Cloud::for_test(&format!("http://{}", listener.local_addr().unwrap()));
        let task = tokio::spawn(async move {
            let mut requests = Vec::new();
            for (expected, status, body) in responses {
                let (mut stream, _) =
                    tokio::time::timeout(Duration::from_secs(5), listener.accept())
                        .await
                        .unwrap()
                        .unwrap();
                let mut data = Vec::new();
                let mut chunk = [0; 4096];
                loop {
                    let n = stream.read(&mut chunk).await.unwrap();
                    assert!(n > 0);
                    data.extend_from_slice(&chunk[..n]);
                    if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&data[..end]);
                        let length: usize = header
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|s| s.trim().parse().unwrap())
                            })
                            .unwrap_or(0);
                        if data.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                let request = String::from_utf8(data).unwrap();
                assert!(
                    request.lines().next().unwrap().contains(expected),
                    "unexpected test route"
                );
                requests.push(request);
                let body = body.to_string();
                let response = format!("HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                stream.write_all(response.as_bytes()).await.unwrap();
            }
            requests
        });
        (cloud, task)
    }
    #[test]
    fn upload_uses_owner_path_and_server_timestamp() {
        let body = upload_body("alice", "mac-1", "Alice’s Mac", "fake-key");
        assert_eq!(
            body["writes"][0]["update"]["name"],
            format!("{}/chat_activation_keys/alice/keys/mac-1", document_root())
        );
        assert_eq!(
            body["writes"][0]["updateTransforms"][0]["setToServerValue"],
            "REQUEST_TIME"
        );
    }
    #[test]
    fn rejects_path_injection() {
        for id in ["", ".", "..", "a/b", "a?b", "a%2fb"] {
            assert!(!valid_id(id));
        }
        assert!(valid_id("firebase-UID_123"));
    }
}
