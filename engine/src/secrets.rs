//! Passwords and OAuth refresh tokens. They live only in the Secret Service keyring, or in
//! memory for the session when no keyring answers; nothing secret is written anywhere else.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Mutex;
use std::time::Duration;
use zeroize::Zeroizing;

pub type Secret = Zeroizing<Vec<u8>>;

/// How long to wait for the Secret Service before falling back to memory.
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SecretKind {
    Password,
    RefreshToken,
}

impl SecretKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::RefreshToken => "refresh-token",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("the keyring is locked")]
    Locked,
    #[error("no keyring answered within {0:?}")]
    Timeout(Duration),
    #[error("keyring error: {0}")]
    Keyring(String),
}

pub trait SecretStore: Send + Sync {
    /// Stores or replaces the secret of `kind` for `account`. `label` is what keyring browsers show.
    fn store(
        &self,
        account: &str,
        kind: SecretKind,
        label: &str,
        secret: &[u8],
    ) -> impl Future<Output = Result<(), SecretError>> + Send;

    fn read(
        &self,
        account: &str,
        kind: SecretKind,
    ) -> impl Future<Output = Result<Option<Secret>, SecretError>> + Send;

    fn delete(
        &self,
        account: &str,
        kind: SecretKind,
    ) -> impl Future<Output = Result<(), SecretError>> + Send;

    /// Removes every secret of `account`, for account removal.
    fn delete_account(&self, account: &str)
    -> impl Future<Output = Result<(), SecretError>> + Send;
}

/// Secrets for this session only: headless tests and the no-keyring fallback.
#[derive(Default)]
pub struct MemoryStore {
    items: Mutex<HashMap<(String, SecretKind), Secret>>,
}

impl MemoryStore {
    fn items(&self) -> std::sync::MutexGuard<'_, HashMap<(String, SecretKind), Secret>> {
        self.items
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl SecretStore for MemoryStore {
    async fn store(
        &self,
        account: &str,
        kind: SecretKind,
        _label: &str,
        secret: &[u8],
    ) -> Result<(), SecretError> {
        self.items()
            .insert((account.to_owned(), kind), Zeroizing::new(secret.to_vec()));
        Ok(())
    }

    async fn read(&self, account: &str, kind: SecretKind) -> Result<Option<Secret>, SecretError> {
        Ok(self.items().get(&(account.to_owned(), kind)).cloned())
    }

    async fn delete(&self, account: &str, kind: SecretKind) -> Result<(), SecretError> {
        self.items().remove(&(account.to_owned(), kind));
        Ok(())
    }

    async fn delete_account(&self, account: &str) -> Result<(), SecretError> {
        self.items().retain(|(owner, _), _| owner != account);
        Ok(())
    }
}

/// The Secret Service (gnome-keyring on Omarchy), or the portal's file keyring when sandboxed.
pub struct KeyringStore {
    keyring: oo7::Keyring,
    application: String,
}

impl KeyringStore {
    /// `application` is the app-id; it scopes every item, so debug and release builds and
    /// tests never see each other's secrets.
    pub async fn open(application: &str) -> Result<Self, SecretError> {
        let keyring = tokio::time::timeout(OPEN_TIMEOUT, oo7::Keyring::new())
            .await
            .map_err(|_| SecretError::Timeout(OPEN_TIMEOUT))?
            .map_err(keyring_error)?;
        let store = Self {
            keyring,
            application: application.to_owned(),
        };
        // Only the installed app migrates real credentials. Debug builds and test IDs
        // keep their existing isolation from the user's accounts.
        // A failed copy must not cost the session its keyring: secrets kept in memory would be
        // gone at the next start. The marker is written only by a completed pass and each item
        // is copied only where missing, so the next launch tries again.
        if application == "io.github.omarsimon1.Pronto"
            && let Err(error) = store.migrate_from("io.github.omarsimon1.Dispatchy").await
        {
            tracing::warn!(
                "could not copy Dispatchy's keyring items, trying again at the next launch: {error}"
            );
        }
        Ok(store)
    }

    /// Copy each missing password and refresh token, then leave a marker in the keyring.
    /// This makes retries safe and prevents a later sign-out/removal from resurrecting
    /// an old credential. Nothing, including the marker, is written to a plain file.
    async fn migrate_from(&self, legacy: &str) -> Result<(), SecretError> {
        self.ensure_unlocked().await?;
        let marker = [
            ("application", self.application.as_str()),
            ("migration", "dispatchy-v1"),
        ];
        if !self
            .keyring
            .search_items(&marker)
            .await
            .map_err(keyring_error)?
            .is_empty()
        {
            return Ok(());
        }
        let items = self
            .keyring
            .search_items(&[("application", legacy)])
            .await
            .map_err(keyring_error)?;
        for item in items {
            let attributes = item.attributes().await.map_err(keyring_error)?;
            let Some(account) = attributes.get("account") else {
                continue;
            };
            let kind = match attributes.get("kind").map(String::as_str) {
                Some("password") => SecretKind::Password,
                Some("refresh-token") => SecretKind::RefreshToken,
                _ => continue,
            };
            let target = self.attributes(account, kind);
            if self
                .keyring
                .search_items(&target)
                .await
                .map_err(keyring_error)?
                .is_empty()
            {
                let secret = item.secret().await.map_err(keyring_error)?;
                self.keyring
                    .create_item("Pronto migrated account", &target, secret, false)
                    .await
                    .map_err(keyring_error)?;
            }
        }
        self.keyring
            .create_item(
                "Pronto migration completed",
                &marker,
                oo7::Secret::blob(b"complete"),
                true,
            )
            .await
            .map_err(keyring_error)
    }

    async fn ensure_unlocked(&self) -> Result<(), SecretError> {
        if self.keyring.is_locked().await.map_err(keyring_error)? {
            self.keyring
                .unlock()
                .await
                .map_err(|_| SecretError::Locked)?;
        }
        Ok(())
    }

    fn attributes<'a>(&'a self, account: &'a str, kind: SecretKind) -> [(&'a str, &'a str); 3] {
        [
            ("application", self.application.as_str()),
            ("account", account),
            ("kind", kind.as_str()),
        ]
    }
}

impl SecretStore for KeyringStore {
    async fn store(
        &self,
        account: &str,
        kind: SecretKind,
        label: &str,
        secret: &[u8],
    ) -> Result<(), SecretError> {
        self.ensure_unlocked().await?;
        self.keyring
            .create_item(
                label,
                &self.attributes(account, kind),
                oo7::Secret::blob(secret),
                true,
            )
            .await
            .map_err(keyring_error)
    }

    async fn read(&self, account: &str, kind: SecretKind) -> Result<Option<Secret>, SecretError> {
        self.ensure_unlocked().await?;
        let items = self
            .keyring
            .search_items(&self.attributes(account, kind))
            .await
            .map_err(keyring_error)?;
        let Some(item) = items.first() else {
            return Ok(None);
        };
        let secret = item.secret().await.map_err(keyring_error)?;
        Ok(Some(Zeroizing::new(secret.as_bytes().to_vec())))
    }

    async fn delete(&self, account: &str, kind: SecretKind) -> Result<(), SecretError> {
        self.ensure_unlocked().await?;
        self.keyring
            .delete(&self.attributes(account, kind))
            .await
            .map_err(keyring_error)
    }

    async fn delete_account(&self, account: &str) -> Result<(), SecretError> {
        self.ensure_unlocked().await?;
        let attributes = [
            ("application", self.application.as_str()),
            ("account", account),
        ];
        self.keyring
            .delete(&attributes)
            .await
            .map_err(keyring_error)
    }
}

fn keyring_error(error: oo7::Error) -> SecretError {
    SecretError::Keyring(error.to_string())
}

/// The keyring when one answers, otherwise memory for the session. The app shows a banner when
/// `is_persistent` is false.
pub enum Secrets {
    Keyring(KeyringStore),
    Memory(MemoryStore),
}

impl Secrets {
    pub async fn open(application: &str) -> Self {
        match KeyringStore::open(application).await {
            Ok(store) => Self::Keyring(store),
            Err(error) => {
                tracing::warn!("no keyring, secrets stay in memory for this session: {error}");
                Self::Memory(MemoryStore::default())
            }
        }
    }

    pub fn is_persistent(&self) -> bool {
        matches!(self, Self::Keyring(_))
    }
}

impl SecretStore for Secrets {
    async fn store(
        &self,
        account: &str,
        kind: SecretKind,
        label: &str,
        secret: &[u8],
    ) -> Result<(), SecretError> {
        match self {
            Self::Keyring(store) => store.store(account, kind, label, secret).await,
            Self::Memory(store) => store.store(account, kind, label, secret).await,
        }
    }

    async fn read(&self, account: &str, kind: SecretKind) -> Result<Option<Secret>, SecretError> {
        match self {
            Self::Keyring(store) => store.read(account, kind).await,
            Self::Memory(store) => store.read(account, kind).await,
        }
    }

    async fn delete(&self, account: &str, kind: SecretKind) -> Result<(), SecretError> {
        match self {
            Self::Keyring(store) => store.delete(account, kind).await,
            Self::Memory(store) => store.delete(account, kind).await,
        }
    }

    async fn delete_account(&self, account: &str) -> Result<(), SecretError> {
        match self {
            Self::Keyring(store) => store.delete_account(account).await,
            Self::Memory(store) => store.delete_account(account).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime() -> tokio::runtime::Runtime {
        crate::runtime().unwrap()
    }

    async fn round_trip(store: &impl SecretStore) {
        let account = "test-account-0001";
        store
            .store(
                account,
                SecretKind::Password,
                "Pronto test password",
                b"hunter2",
            )
            .await
            .unwrap();
        store
            .store(
                account,
                SecretKind::RefreshToken,
                "Pronto test token",
                b"token-1",
            )
            .await
            .unwrap();
        store
            .store(
                account,
                SecretKind::RefreshToken,
                "Pronto test token",
                b"token-2",
            )
            .await
            .unwrap();
        let password = store.read(account, SecretKind::Password).await.unwrap();
        assert_eq!(
            password.as_deref().map(Vec::as_slice),
            Some(&b"hunter2"[..])
        );
        let token = store.read(account, SecretKind::RefreshToken).await.unwrap();
        assert_eq!(token.as_deref().map(Vec::as_slice), Some(&b"token-2"[..]));

        store.delete(account, SecretKind::Password).await.unwrap();
        assert!(
            store
                .read(account, SecretKind::Password)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .read(account, SecretKind::RefreshToken)
                .await
                .unwrap()
                .is_some()
        );

        store.delete_account(account).await.unwrap();
        assert!(
            store
                .read(account, SecretKind::RefreshToken)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn memory_store_round_trip() {
        runtime().block_on(round_trip(&MemoryStore::default()));
    }

    #[test]
    fn memory_store_keeps_accounts_apart() {
        runtime().block_on(async {
            let store = MemoryStore::default();
            store
                .store("a", SecretKind::Password, "", b"one")
                .await
                .unwrap();
            store
                .store("b", SecretKind::Password, "", b"two")
                .await
                .unwrap();
            store.delete_account("a").await.unwrap();
            assert!(
                store
                    .read("a", SecretKind::Password)
                    .await
                    .unwrap()
                    .is_none()
            );
            let other = store.read("b", SecretKind::Password).await.unwrap();
            assert_eq!(other.as_deref().map(Vec::as_slice), Some(&b"two"[..]));
        });
    }

    #[test]
    #[ignore = "needs a private Secret Service (run under tools/headless.sh)"]
    fn keyring_migration_copies_missing_items_and_never_resurrects_deleted_ones() {
        runtime().block_on(async {
            let old = KeyringStore::open("io.github.omarsimon1.Dispatchy.MigrationTest")
                .await
                .unwrap();
            let new = KeyringStore::open("io.github.omarsimon1.Pronto.MigrationTest")
                .await
                .unwrap();
            for store in [&old, &new] {
                store
                    .keyring
                    .delete(&[("application", store.application.as_str())])
                    .await
                    .unwrap();
            }
            old.store("a", SecretKind::Password, "fictional", b"old-password")
                .await
                .unwrap();
            old.store("a", SecretKind::RefreshToken, "fictional", b"old-token")
                .await
                .unwrap();
            new.store("a", SecretKind::Password, "fictional", b"new-password")
                .await
                .unwrap();
            new.migrate_from(&old.application).await.unwrap();
            assert_eq!(
                new.read("a", SecretKind::Password)
                    .await
                    .unwrap()
                    .unwrap()
                    .as_slice(),
                b"new-password"
            );
            assert_eq!(
                new.read("a", SecretKind::RefreshToken)
                    .await
                    .unwrap()
                    .unwrap()
                    .as_slice(),
                b"old-token"
            );
            assert_eq!(
                old.read("a", SecretKind::Password)
                    .await
                    .unwrap()
                    .unwrap()
                    .as_slice(),
                b"old-password"
            );
            assert_eq!(
                old.read("a", SecretKind::RefreshToken)
                    .await
                    .unwrap()
                    .unwrap()
                    .as_slice(),
                b"old-token"
            );
            new.delete_account("a").await.unwrap();
            new.migrate_from(&old.application).await.unwrap();
            assert!(
                new.read("a", SecretKind::RefreshToken)
                    .await
                    .unwrap()
                    .is_none()
            );
            for store in [&old, &new] {
                store
                    .keyring
                    .delete(&[("application", store.application.as_str())])
                    .await
                    .unwrap();
            }
        });
    }

    /// Talks to the live Secret Service; run with
    /// `cargo test -p sealwax-engine -- --ignored keyring_round_trip` in a desktop session.
    #[test]
    #[ignore = "needs the session's Secret Service"]
    fn keyring_round_trip() {
        runtime().block_on(async {
            let store = KeyringStore::open("io.github.omarsimon1.Pronto.Devel.test")
                .await
                .unwrap();
            round_trip(&store).await;
        });
    }

    /// Run on a bus without a Secret Service, as `tools/headless.sh` provides:
    /// `tools/headless.sh <display> cargo test -p sealwax-engine -- --ignored falls_back`.
    #[test]
    #[ignore = "needs a session bus without a Secret Service"]
    fn falls_back_to_memory_without_a_keyring() {
        runtime().block_on(async {
            let secrets = Secrets::open("io.github.omarsimon1.Pronto.Devel.test").await;
            assert!(!secrets.is_persistent());
            round_trip(&secrets).await;
        });
    }
}
