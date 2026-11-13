//! Microsoft sign-in (ML-032; ADR-008 and its S5 amendment), ported from spike S5, which proved
//! the flow live on 2026-10-05: the authorization-code flow with PKCE (S256) in the system
//! browser and a loopback redirect, the code exchange and refreshes at Microsoft's `common`
//! endpoints, the username from the ID token, and a per-account cache of access tokens that
//! refreshes from the keyring and stores every rotated refresh token back.
//!
//! Google sign-in (ADR-023) is the same flow with Google's endpoints and scope
//! (`https://mail.google.com/`), a `http://127.0.0.1:<port>` redirect, `access_type=offline`
//! for a refresh token, the optional client secret Google gives Desktop app clients, and the
//! username from the ID token's verified `email`.
//!
//! Nothing here logs or displays a token, a code, a verifier, an ID token, a username, a client
//! secret or the authorize URL. Errors carry no server text: only the OAuth error's class and
//! Microsoft's numeric AADSTS code.

mod access;
mod error;
mod flow;
mod id_token;
mod token;

pub use access::{AccessToken, AccessTokens, Fetched, MARGIN};
pub use error::OAuthError;
pub use flow::Authorization;
pub use id_token::{CONSUMER_TENANT, Identity};
pub use token::Tokens;

use crate::net::Trust;
use sealwax_domain::account::OAuthProvider;
use std::fmt;
use std::time::Duration;
use url::Url;
use zeroize::Zeroizing;

/// What Pronto asks Microsoft for: IMAP and SMTP as the signed-in user, a refresh token, and an
/// ID token that names the user. The Outlook scopes come first, because Microsoft issues the
/// access token for the resource of the first scope when a request names several.
pub const MICROSOFT_SCOPES: &str = "https://outlook.office.com/IMAP.AccessAsUser.All \
    https://outlook.office.com/SMTP.Send offline_access openid profile";

/// What Pronto asks Google for: IMAP and SMTP (Gmail's XOAUTH2 takes only this full-mail
/// scope), and an ID token with the user's address.
pub const GOOGLE_SCOPES: &str = "https://mail.google.com/ openid email";
/// The scope Gmail's IMAP and SMTP need.
pub const GOOGLE_MAIL_SCOPE: &str = "https://mail.google.com/";

/// Connect and answer together, for every token request.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// An authorization server: whose it is, which decides the scopes, the redirect's form and how
/// the ID token is checked, and its two endpoints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provider {
    kind: OAuthProvider,
    authorize: Url,
    token: Url,
}

impl Provider {
    /// Microsoft's `common` endpoints, for work, school and personal accounts alike.
    pub fn microsoft() -> Self {
        Self::new(
            OAuthProvider::Microsoft,
            "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
            "https://login.microsoftonline.com/common/oauth2/v2.0/token",
        )
        .expect("Microsoft's endpoints are HTTPS URLs")
    }

    /// Google's endpoints (developers.google.com/identity/protocols/oauth2/native-app).
    pub fn google() -> Self {
        Self::new(
            OAuthProvider::Google,
            "https://accounts.google.com/o/oauth2/v2/auth",
            "https://oauth2.googleapis.com/token",
        )
        .expect("Google's endpoints are HTTPS URLs")
    }

    /// Endpoints must be HTTPS; plain HTTP is accepted only on a loopback address, for tests.
    pub fn new(kind: OAuthProvider, authorize: &str, token: &str) -> Result<Self, OAuthError> {
        Ok(Self {
            kind,
            authorize: endpoint(authorize)?,
            token: endpoint(token)?,
        })
    }

    pub fn kind(&self) -> OAuthProvider {
        self.kind
    }

    /// The scopes a sign-in asks for.
    pub fn scopes(&self) -> &'static str {
        match self.kind {
            OAuthProvider::Microsoft => MICROSOFT_SCOPES,
            OAuthProvider::Google => GOOGLE_SCOPES,
        }
    }
}

/// A client registration as configured (ADR-008, ADR-023): the client ID and, for Google's
/// Desktop app clients, the client secret Google issues with it, which Google documents as not
/// confidential for installed apps but which is still never logged. Its debug form shows
/// neither.
#[derive(Clone, PartialEq, Eq)]
pub struct Registration {
    pub id: String,
    pub secret: Option<String>,
}

impl Registration {
    /// A registration from configured text: blanks count as none, and no ID means none.
    pub fn new(id: Option<&str>, secret: Option<&str>) -> Option<Self> {
        let usable = |text: &str| {
            let text = text.trim();
            (!text.is_empty()).then(|| text.to_owned())
        };
        Some(Self {
            id: id.and_then(usable)?,
            secret: secret.and_then(usable),
        })
    }
}

impl fmt::Debug for Registration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Registration")
            .field("secret", &self.secret.is_some())
            .finish_non_exhaustive()
    }
}

fn endpoint(text: &str) -> Result<Url, OAuthError> {
    let url = Url::parse(text).map_err(|_| OAuthError::Endpoint)?;
    let loopback = match url.host() {
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        _ => false,
    };
    match url.scheme() {
        "https" => Ok(url),
        "http" if loopback => Ok(url),
        _ => Err(OAuthError::Endpoint),
    }
}

/// A registered public client (desktop clients use PKCE; Google's also send the secret it
/// issued them) and the HTTP client its token requests go through: rustls with the platform
/// verifier, no proxy, no redirects.
pub struct Client {
    id: String,
    secret: Option<Zeroizing<String>>,
    provider: Provider,
    http: reqwest::Client,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("provider", &self.provider)
            .finish_non_exhaustive()
    }
}

impl Client {
    pub fn new(id: String, provider: Provider) -> Result<Self, OAuthError> {
        let tls = Trust::Platform
            .client_config()
            .map_err(|_| OAuthError::Setup)?;
        let http = reqwest::Client::builder()
            .user_agent(concat!("Pronto/", env!("CARGO_PKG_VERSION")))
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .tls_backend_preconfigured((*tls).clone())
            .connect_timeout(HTTP_TIMEOUT)
            .timeout(HTTP_TIMEOUT)
            .build()
            .map_err(|_| OAuthError::Setup)?;
        Ok(Self {
            id,
            secret: None,
            provider,
            http,
        })
    }

    /// A client for a configured registration.
    pub fn registered(registration: &Registration, provider: Provider) -> Result<Self, OAuthError> {
        Ok(Self::new(registration.id.clone(), provider)?
            .with_secret(registration.secret.clone().map(Zeroizing::new)))
    }

    /// Sends `client_secret` with every token request (Google's Desktop app clients).
    pub fn with_secret(mut self, secret: Option<Zeroizing<String>>) -> Self {
        self.secret = secret.filter(|secret| !secret.trim().is_empty());
        self
    }

    pub fn provider(&self) -> OAuthProvider {
        self.provider.kind
    }
}

/// A finished sign-in: who signed in, and the tokens. Its debug form shows neither.
pub struct Granted {
    pub identity: Identity,
    /// Always with a refresh token.
    pub tokens: Tokens,
}

impl fmt::Debug for Granted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Granted")
            .field("identity", &self.identity)
            .field("tokens", &self.tokens)
            .finish()
    }
}

#[cfg(test)]
mod google_tests;
#[cfg(test)]
mod tests;
