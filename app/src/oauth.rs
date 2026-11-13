//! The OAuth clients Pronto signs in with (ADR-008, OAUTH-SETUP.md; ADR-023,
//! GOOGLE-OAUTH-SETUP.md). Microsoft's is public, so only its client ID is needed:
//! `[oauth.microsoft] client_id` in `config.toml` overrides the one the build was given. Google's
//! is the user's own Desktop app client: `[oauth.google]` in `config.toml`, else
//! `PRONTO_GOOGLE_CLIENT_ID` (and `PRONTO_GOOGLE_CLIENT_SECRET`) in the environment Pronto
//! runs in, else the same variables as they were when it was built. Nothing here is committed to
//! the repository, and neither an ID nor a secret is ever logged.

use sealwax_domain::config::Config;
use sealwax_engine::oauth::Registration;

/// `PRONTO_MICROSOFT_CLIENT_ID` as it was when Pronto was built, if it was set.
pub const BUILD_MICROSOFT_CLIENT_ID: Option<&str> = option_env!("PRONTO_MICROSOFT_CLIENT_ID");

/// The environment variables a Google client is read from, at run time and at build time.
pub const GOOGLE_CLIENT_ID_ENV: &str = "PRONTO_GOOGLE_CLIENT_ID";
pub const GOOGLE_CLIENT_SECRET_ENV: &str = "PRONTO_GOOGLE_CLIENT_SECRET";

/// `PRONTO_GOOGLE_CLIENT_ID` and `PRONTO_GOOGLE_CLIENT_SECRET` as they were when Pronto was
/// built, if set.
pub const BUILD_GOOGLE_CLIENT_ID: Option<&str> = option_env!("PRONTO_GOOGLE_CLIENT_ID");
pub const BUILD_GOOGLE_CLIENT_SECRET: Option<&str> = option_env!("PRONTO_GOOGLE_CLIENT_SECRET");

/// The Google client from Pronto's environment, else from its build (above).
pub fn google_client_from_env() -> Option<Registration> {
    let id = std::env::var(GOOGLE_CLIENT_ID_ENV).ok();
    let secret = std::env::var(GOOGLE_CLIENT_SECRET_ENV).ok();
    Registration::new(id.as_deref(), secret.as_deref())
        .or_else(|| Registration::new(BUILD_GOOGLE_CLIENT_ID, BUILD_GOOGLE_CLIENT_SECRET))
}

/// The Google client in use: `config.toml`'s `[oauth.google]` when it has a client ID (with
/// its secret, if any), else `fallback` (the environment's or the build's). A section's secret
/// never pairs with another source's ID.
pub fn google_client(config: &Config, fallback: Option<&Registration>) -> Option<Registration> {
    let configured = &config.oauth.google;
    Registration::new(
        configured.client_id.as_deref(),
        configured.client_secret.as_deref(),
    )
    .or_else(|| fallback.cloned())
}

/// The Microsoft client ID in use: `config.toml`'s, else `built_in` (the build's). A blank one
/// counts as none, so Microsoft sign-in is unavailable.
pub fn microsoft_client_id(config: &Config, built_in: Option<&str>) -> Option<String> {
    let usable = |id: &str| {
        let id = id.trim();
        (!id.is_empty()).then(|| id.to_owned())
    };
    config
        .oauth
        .microsoft
        .client_id
        .as_deref()
        .and_then(usable)
        .or_else(|| built_in.and_then(usable))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google_takes_config_toml_then_the_environment() {
        let none = Config::default();
        assert_eq!(google_client(&none, None), None);
        let fallback = Registration::new(Some("env-id"), Some("env-secret"));
        assert_eq!(google_client(&none, fallback.as_ref()), fallback);

        let configured = Config::parse(
            "[oauth.google]\nclient_id = \"file-id\"\nclient_secret = \"file-secret\"\n",
        )
        .expect("a valid config.toml");
        let found = google_client(&configured, fallback.as_ref()).unwrap();
        assert_eq!(found.id, "file-id");
        assert_eq!(found.secret.as_deref(), Some("file-secret"));

        // An ID alone in the file goes without a secret; the environment's isn't borrowed.
        let id_only = Config::parse("[oauth.google]\nclient_id = \"file-id\"\n").unwrap();
        let found = google_client(&id_only, fallback.as_ref()).unwrap();
        assert_eq!((found.id.as_str(), found.secret), ("file-id", None));

        // A secret without an ID in the file doesn't count; the fallback stays whole.
        let secret_only = Config::parse("[oauth.google]\nclient_secret = \"x\"\n").unwrap();
        assert_eq!(google_client(&secret_only, fallback.as_ref()), fallback);
    }

    #[test]
    fn config_toml_wins_over_the_build() {
        let none = Config::default();
        assert_eq!(microsoft_client_id(&none, None), None);
        assert_eq!(microsoft_client_id(&none, Some("  ")), None);
        assert_eq!(
            microsoft_client_id(&none, Some(" built-in-id ")).as_deref(),
            Some("built-in-id")
        );
        let configured = Config::parse("[oauth.microsoft]\nclient_id = \"configured-id\"\n")
            .expect("a valid config.toml");
        assert_eq!(
            microsoft_client_id(&configured, Some("built-in-id")).as_deref(),
            Some("configured-id")
        );
        assert_eq!(
            microsoft_client_id(&configured, None).as_deref(),
            Some("configured-id")
        );
    }
}
