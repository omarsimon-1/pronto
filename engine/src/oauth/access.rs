//! Access tokens for syncing accounts: one per account, in memory only. A token with less than
//! [`MARGIN`] left, or one a server refused, is replaced through the refresh token in the
//! keyring, and the rotated refresh token that comes back is stored over it (S5: every refresh
//! rotates it). An account refreshes once at a time.

use super::Client;
use crate::secrets::{SecretKind, SecretStore};
use sealwax_domain::account::{AccountKey, LoginFailure};
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime};
use zeroize::Zeroizing;

/// A token with less than this left is refreshed before use.
pub const MARGIN: Duration = Duration::from_secs(300);

/// An access token and when it expires. Its debug form shows no token.
pub struct AccessToken {
    token: Zeroizing<String>,
    /// By the monotonic clock, which a clock set back can't stretch.
    expires: Instant,
    /// By the wall clock: the token's life goes on while the machine is suspended, which
    /// the monotonic clock doesn't count.
    expires_at: SystemTime,
}

impl AccessToken {
    /// A token issued just now that lasts `lifetime`.
    pub fn new(token: Zeroizing<String>, lifetime: Duration) -> Self {
        Self {
            token,
            expires: Instant::now() + lifetime,
            expires_at: SystemTime::now() + lifetime,
        }
    }

    /// More than [`MARGIN`] left by both clocks.
    fn usable(&self) -> bool {
        self.usable_at(Instant::now(), SystemTime::now())
    }

    fn usable_at(&self, now: Instant, wall: SystemTime) -> bool {
        self.expires.saturating_duration_since(now) > MARGIN
            && self
                .expires_at
                .duration_since(wall)
                .is_ok_and(|left| left > MARGIN)
    }
}

impl fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccessToken(..)")
    }
}

/// A token to log in with.
pub struct Fetched {
    pub token: Zeroizing<String>,
    /// It comes from a refresh just now, so a server's refusal isn't worth another refresh.
    pub refreshed: bool,
}

type Slot = Arc<tokio::sync::Mutex<Option<AccessToken>>>;

/// Every account's access token.
#[derive(Default)]
pub struct AccessTokens {
    slots: Mutex<HashMap<AccountKey, Slot>>,
}

impl AccessTokens {
    fn slots(&self) -> MutexGuard<'_, HashMap<AccountKey, Slot>> {
        self.slots.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Keeps a sign-in's access token for the account.
    pub fn seed(&self, key: &AccountKey, token: AccessToken) {
        self.slots()
            .insert(key.clone(), Arc::new(tokio::sync::Mutex::new(Some(token))));
    }

    /// Drops the account's token (a removed account).
    pub fn forget(&self, key: &AccountKey) {
        self.slots().remove(key);
    }

    /// The account's token: the one held while it has more than [`MARGIN`] left and `renew`
    /// isn't set, otherwise a new one from the refresh token in `secrets`, whose rotated
    /// successor is stored there under `label`. No refresh token, no client, or a refused
    /// refresh means the account must sign in again (`OAuthRequired`).
    pub async fn get<S: SecretStore>(
        &self,
        key: &AccountKey,
        label: &str,
        secrets: &S,
        client: Option<&Client>,
        renew: bool,
    ) -> Result<Fetched, LoginFailure> {
        let slot = self.slots().entry(key.clone()).or_default().clone();
        let mut held = slot.lock().await;
        if !renew && let Some(token) = held.as_ref().filter(|token| token.usable()) {
            return Ok(Fetched {
                token: token.token.clone(),
                refreshed: false,
            });
        }
        *held = None;
        let client = client.ok_or(LoginFailure::OAuthRequired)?;
        let stored = secrets
            .read(&key.0, SecretKind::RefreshToken)
            .await
            .map_err(|error| {
                tracing::warn!(%error, "the refresh token couldn't be read");
                LoginFailure::Other
            })?
            .ok_or(LoginFailure::OAuthRequired)?;
        let stored = std::str::from_utf8(&stored).map_err(|_| LoginFailure::OAuthRequired)?;
        let tokens = client.refresh(stored).await.map_err(|error| {
            tracing::info!(%error, "the access token couldn't be refreshed");
            error.failure()
        })?;
        if let Some(rotated) = tokens.refresh.as_ref().filter(|new| new.as_str() != stored) {
            match secrets
                .store(&key.0, SecretKind::RefreshToken, label, rotated.as_bytes())
                .await
            {
                Ok(()) => tracing::debug!("the rotated refresh token replaced the stored one"),
                Err(error) => {
                    tracing::warn!(%error, "the rotated refresh token couldn't be stored")
                }
            }
        }
        let token = AccessToken::new(tokens.access, tokens.lifetime);
        let fetched = Fetched {
            token: token.token.clone(),
            refreshed: true,
        };
        *held = Some(token);
        Ok(fetched)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_expires_by_the_wall_clock_across_a_suspend() {
        let hour = Duration::from_secs(3600);
        let token = AccessToken::new(Zeroizing::new("fictional".into()), hour);
        let (now, wall) = (Instant::now(), SystemTime::now());
        assert!(token.usable());
        assert!(token.usable_at(now + hour - 2 * MARGIN, wall + hour - 2 * MARGIN));
        assert!(!token.usable_at(now + hour, wall + hour));
        // Two hours asleep: the monotonic clock stood still, the wall clock didn't.
        assert!(!token.usable_at(now, wall + 2 * hour));
        assert!(!token.usable_at(now, wall + hour - MARGIN));
        // A wall clock set back gives the token no longer life.
        assert!(!token.usable_at(now + hour, wall - hour));
    }
}
