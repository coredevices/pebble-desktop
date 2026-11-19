use crate::cloud::{Cloud, Error, SavedKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Sync,
    Enable,
    Disable,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct AccountSettings {
    disabled: bool,
    pending_removal: bool,
    fingerprint: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Preferences {
    installation_id: String,
    accounts: BTreeMap<String, AccountSettings>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            installation_id: uuid::Uuid::new_v4().to_string(),
            accounts: BTreeMap::new(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyStatus {
    pub enabled: bool,
    pub computer_name: String,
    pub saved_at: Option<String>,
}

pub struct Activation {
    path: PathBuf,
    prefs: Preferences,
}

impl Activation {
    pub fn load(path: PathBuf) -> Result<Self, Error> {
        let prefs = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<Preferences>(&bytes).map_err(Error::local)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Preferences::default(),
            Err(error) => return Err(Error::local(error)),
        };
        if uuid::Uuid::parse_str(&prefs.installation_id).is_err() {
            return Err(Error::local("invalid installation"));
        }
        let service = Self { path, prefs };
        service.save()?;
        Ok(service)
    }

    fn save(&self) -> Result<(), Error> {
        use std::io::Write;
        let parent = self.path.parent().ok_or(Error::local("no directory"))?;
        std::fs::create_dir_all(parent).map_err(Error::local)?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let temporary = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut file = options.open(&temporary).map_err(Error::local)?;
            file.write_all(&serde_json::to_vec(&self.prefs).map_err(Error::local)?)
                .map_err(Error::local)?;
            file.sync_all().map_err(Error::local)?;
            std::fs::rename(&temporary, &self.path).map_err(Error::local)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result
    }

    pub async fn sync(
        &mut self,
        cloud: &Cloud,
        token: &str,
        uid: &str,
        operation: Operation,
        name: String,
        extract: impl FnOnce() -> Result<String, Error> + Send + 'static,
    ) -> Result<KeyStatus, Error> {
        // This check MUST precede extraction and any key writes, including on fresh installs.
        cloud.account(token, Some(uid)).await?;
        let settings = self.prefs.accounts.entry(uid.into()).or_default();
        match operation {
            Operation::Enable => {
                settings.disabled = false;
                settings.pending_removal = false;
            }
            Operation::Disable => {
                settings.disabled = true;
                settings.pending_removal = true;
            }
            Operation::Sync => {}
        }
        self.save()?;
        let settings = &self.prefs.accounts[uid];
        let installation = &self.prefs.installation_id;
        if settings.disabled {
            if settings.pending_removal {
                cloud.remove(token, uid, installation).await?;
                let settings = self.prefs.accounts.get_mut(uid).expect("account inserted");
                settings.pending_removal = false;
                settings.fingerprint = None;
                self.save()?;
            }
            return Ok(KeyStatus {
                enabled: false,
                computer_name: name,
                saved_at: None,
            });
        }
        let key = tauri::async_runtime::spawn_blocking(extract)
            .await
            .map_err(Error::local)??;
        let fingerprint = format!("{:x}", Sha256::digest(key.as_bytes()));
        let existing = if settings.fingerprint.as_ref() == Some(&fingerprint) {
            cloud
                .metadata(token, uid, installation)
                .await?
                .filter(|saved| saved.computer_name == name)
        } else {
            None
        };
        let saved: SavedKey = match existing {
            Some(saved) => saved,
            None => cloud.upload(token, uid, installation, &name, &key).await?,
        };
        self.prefs
            .accounts
            .get_mut(uid)
            .expect("account inserted")
            .fingerprint = Some(fingerprint);
        self.save()?;
        Ok(KeyStatus {
            enabled: true,
            computer_name: saved.computer_name,
            saved_at: Some(saved.saved_at),
        })
    }
}

pub fn preferences_path(config_dir: &Path) -> PathBuf {
    config_dir.join("imessage.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::tests::serve;
    use serde_json::json;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    fn lookup() -> (&'static str, u16, serde_json::Value) {
        (
            "accounts:lookup",
            200,
            json!({"users":[{"localId":"alice"}]}),
        )
    }
    fn existing_user() -> (&'static str, u16, serde_json::Value) {
        ("/users/alice", 200, json!({"fields":{}}))
    }

    #[tokio::test]
    async fn missing_account_and_permission_failures_never_extract_or_upload() {
        for status in [404, 403, 500] {
            let (cloud, requests) =
                serve(vec![lookup(), ("/users/alice", status, json!({}))]).await;
            let dir = tempfile::tempdir().unwrap();
            let mut service = Activation::load(preferences_path(dir.path())).unwrap();
            let result = service
                .sync(
                    &cloud,
                    "test-token",
                    "alice",
                    Operation::Sync,
                    "Mac".into(),
                    || panic!("must not extract"),
                )
                .await;
            let error = result.err().unwrap();
            assert_eq!(
                error.code,
                match status {
                    404 => "no_pebble_account",
                    403 => "permission",
                    _ => "cloud",
                }
            );
            assert_eq!(requests.await.unwrap().len(), 2);
            assert!(service.prefs.accounts.is_empty());
        }
    }

    #[tokio::test]
    async fn uid_mismatch_never_reads_the_other_accounts_keys() {
        let (cloud, requests) = serve(vec![lookup()]).await;
        let dir = tempfile::tempdir().unwrap();
        let mut service = Activation::load(preferences_path(dir.path())).unwrap();
        let result = service
            .sync(
                &cloud,
                "alice-token",
                "bob",
                Operation::Sync,
                "Mac".into(),
                || panic!("must not extract"),
            )
            .await;
        assert_eq!(result.err().unwrap().code, "account_changed");
        assert_eq!(requests.await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn enable_restart_disable_and_failed_removal_retry() {
        let (cloud, requests) = serve(vec![
            lookup(), existing_user(), ("documents:commit", 200, json!({"commitTime":"2026-09-17T12:00:00Z"})),
            lookup(), existing_user(), ("/chat_activation_keys/alice/keys/", 200, json!({"fields":{"computerName":{"stringValue":"Mac"},"createdAt":{"timestampValue":"2026-09-17T12:00:00Z"}}})),
            lookup(), existing_user(), ("DELETE ", 500, json!({})),
            lookup(), existing_user(), ("DELETE ", 200, json!({})),
            lookup(), existing_user(),
        ]).await;
        let dir = tempfile::tempdir().unwrap();
        let path = preferences_path(dir.path());
        let mut service = Activation::load(path.clone()).unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        for _ in 0..2 {
            let count = count.clone();
            let status = service
                .sync(
                    &cloud,
                    "token",
                    "alice",
                    Operation::Sync,
                    "Mac".into(),
                    move || {
                        count.fetch_add(1, Ordering::SeqCst);
                        Ok("synthetic-key".into())
                    },
                )
                .await
                .unwrap();
            assert!(status.enabled);
            assert_eq!(status.saved_at.as_deref(), Some("2026-09-17T12:00:00Z"));
            service = Activation::load(path.clone()).unwrap();
        }
        assert_eq!(count.load(Ordering::SeqCst), 2);
        // Failed deletion persists intent, so restarting never turns sharing back on.
        assert!(service
            .sync(
                &cloud,
                "token",
                "alice",
                Operation::Disable,
                "Mac".into(),
                || panic!("disabled")
            )
            .await
            .is_err());
        service = Activation::load(path.clone()).unwrap();
        for _ in 0..2 {
            let status = service
                .sync(
                    &cloud,
                    "token",
                    "alice",
                    Operation::Sync,
                    "Mac".into(),
                    || panic!("disabled"),
                )
                .await
                .unwrap();
            assert!(!status.enabled);
        }
        let requests = requests.await.unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.contains("documents:commit"))
                .count(),
            1
        );
        // Only fingerprint/settings are persisted locally, never the hardware key or ID token.
        let preferences = std::fs::read_to_string(path).unwrap();
        assert!(!preferences.contains("synthetic-key"));
        assert!(!preferences.contains("token"));
    }
    #[test]
    fn defaults_on_and_keeps_preferences_separate_for_each_account() {
        let dir = tempfile::tempdir().unwrap();
        let path = preferences_path(dir.path());
        let mut service = Activation::load(path.clone()).unwrap();
        let id = service.prefs.installation_id.clone();
        service
            .prefs
            .accounts
            .entry("alice".into())
            .or_default()
            .disabled = true;
        service.save().unwrap();
        let mut reloaded = Activation::load(path).unwrap();
        assert_eq!(reloaded.prefs.installation_id, id);
        assert!(reloaded.prefs.accounts["alice"].disabled);
        assert!(
            !reloaded
                .prefs
                .accounts
                .entry("bob".into())
                .or_default()
                .disabled
        );
    }
    #[test]
    fn corrupted_preferences_fail_instead_of_silently_reenabling() {
        let dir = tempfile::tempdir().unwrap();
        let path = preferences_path(dir.path());
        std::fs::write(&path, b"bad").unwrap();
        assert!(Activation::load(path).is_err());
    }
}
