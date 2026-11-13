//! Sign in with Google (ADR-023) against a fake Google on 127.0.0.1, offline: PKCE and the
//! sign-in page's URL, the redirect's query, Google's ID token, the code exchange and refreshes
//! with the client secret, the XOAUTH2 string Gmail takes, and three Gmail accounts whose tokens
//! never mix.

use super::error::authorize_error;
use super::flow::{Callback, callback, challenge, random};
use super::id_token::check;
use super::*;
use crate::secrets::{MemoryStore, SecretKind, SecretStore};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use sealwax_domain::account::{AccountKey, LoginFailure, OAuthProvider};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

const CLIENT: &str = "1234567890-test.apps.googleusercontent.com";
const SECRET: &str = "GOCSPX-test-secret";
const ACCOUNTS: [&str; 3] = ["one@gmail.com", "two@gmail.com", "three@example.org"];

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// A JWT with these claims and a signature nobody checks.
fn jwt(claims: &serde_json::Value) -> String {
    let encode = |text: &str| URL_SAFE_NO_PAD.encode(text);
    format!(
        "{}.{}.{}",
        encode(r#"{"alg":"RS256","typ":"JWT"}"#),
        encode(&claims.to_string()),
        encode("signature")
    )
}

fn google_claims(email: &str) -> serde_json::Value {
    serde_json::json!({
        "iss": "https://accounts.google.com",
        "azp": CLIENT,
        "aud": CLIENT,
        "sub": "110169484474386276334",
        "email": email,
        "email_verified": true,
        "iat": now(),
        "exp": now() + 3600,
    })
}

#[derive(Default)]
struct GoogleState {
    /// Each live refresh token's account.
    refresh: HashMap<String, String>,
    /// Each live access token's account.
    access: HashMap<String, String>,
    /// Every form posted to the token endpoint.
    forms: Vec<HashMap<String, String>>,
    issued: u32,
    lifetime: u64,
    /// A code exchange issues tokens for this account.
    next_user: String,
}

/// A fake Google token endpoint: the code exchange issues a refresh token, an access token and
/// an ID token for `next_user`; a refresh issues only an access token for the refresh token's
/// account and keeps the refresh token, as Google does. Every request must carry the client
/// secret.
struct FakeGoogle {
    port: u16,
    state: Arc<Mutex<GoogleState>>,
}

impl FakeGoogle {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = Arc::new(Mutex::new(GoogleState {
            lifetime: 3599,
            ..GoogleState::default()
        }));
        let shared = state.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve(stream, shared.clone()));
            }
        });
        Self { port, state }
    }

    fn provider(&self) -> Provider {
        Provider::new(
            OAuthProvider::Google,
            &format!("http://127.0.0.1:{}/o/oauth2/v2/auth", self.port),
            &format!("http://127.0.0.1:{}/token", self.port),
        )
        .unwrap()
    }

    fn client(&self) -> Client {
        let registration = Registration::new(Some(CLIENT), Some(SECRET)).unwrap();
        Client::registered(&registration, self.provider()).unwrap()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, GoogleState> {
        self.state.lock().unwrap()
    }

    /// A refresh token for `user`, as if they had signed in.
    fn grant(&self, user: &str) -> String {
        let mut state = self.state();
        state.issued += 1;
        let token = format!("1//refresh-{}-{user}", state.issued);
        state.refresh.insert(token.clone(), user.to_owned());
        token
    }
}

fn answer(status: &str, json: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
         connection: close\r\n\r\n{json}",
        json.len()
    )
}

async fn serve(stream: TcpStream, state: Arc<Mutex<GoogleState>>) {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).await.unwrap();
    let mut length = 0;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).await.unwrap() <= 2 {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap();
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).await.unwrap();
    let form: HashMap<String, String> = url::form_urlencoded::parse(&body).into_owned().collect();
    let response = {
        let mut state = state.lock().unwrap();
        state.forms.push(form.clone());
        let lifetime = state.lifetime;
        if form.get("client_id").map(String::as_str) != Some(CLIENT)
            || form.get("client_secret").map(String::as_str) != Some(SECRET)
        {
            answer(
                "401 Unauthorized",
                r#"{"error":"invalid_client","error_description":"Unauthorized"}"#,
            )
        } else {
            match form["grant_type"].as_str() {
                "authorization_code" => {
                    state.issued += 1;
                    let n = state.issued;
                    let user = state.next_user.clone();
                    let refresh = format!("1//refresh-{n}-{user}");
                    let access = format!("ya29.access-{n}-{user}");
                    state.refresh.insert(refresh.clone(), user.clone());
                    state.access.insert(access.clone(), user.clone());
                    let json = serde_json::json!({
                        "access_token": access,
                        "expires_in": lifetime,
                        "refresh_token": refresh,
                        "scope": "https://mail.google.com/ openid https://www.googleapis.com/auth/userinfo.email",
                        "token_type": "Bearer",
                        "id_token": jwt(&google_claims(&user)),
                    });
                    answer("200 OK", &json.to_string())
                }
                "refresh_token" => match state.refresh.get(&form["refresh_token"]).cloned() {
                    Some(user) => {
                        state.issued += 1;
                        let access = format!("ya29.access-{}-{user}", state.issued);
                        state.access.insert(access.clone(), user);
                        let json = serde_json::json!({
                            "access_token": access,
                            "expires_in": lifetime,
                            "scope": "https://mail.google.com/",
                            "token_type": "Bearer",
                        });
                        answer("200 OK", &json.to_string())
                    }
                    None => answer(
                        "400 Bad Request",
                        r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#,
                    ),
                },
                _ => answer("400 Bad Request", r#"{"error":"unsupported_grant_type"}"#),
            }
        }
    };
    let _ = reader.into_inner().write_all(response.as_bytes()).await;
}

fn query_of(url: &str) -> HashMap<String, String> {
    url::Url::parse(url)
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect()
}

#[test]
fn pkce_verifiers_are_random_and_their_challenges_s256() {
    // RFC 7636's appendix B example.
    assert_eq!(
        challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
    let verifiers: HashSet<String> = (0..32).map(|_| random(64).unwrap().to_string()).collect();
    assert_eq!(verifiers.len(), 32, "every verifier is new");
    for verifier in &verifiers {
        assert!((43..=128).contains(&verifier.len()), "RFC 7636's length");
        assert!(
            verifier
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        );
        let challenge = challenge(verifier);
        assert_eq!(challenge.len(), 43, "a SHA-256, base64url without padding");
        assert_ne!(&challenge, verifier);
    }
}

#[test]
fn the_google_sign_in_page_asks_for_mail_offline_with_pkce() {
    let client = Client::registered(
        &Registration::new(Some(CLIENT), Some(SECRET)).unwrap(),
        Provider::google(),
    )
    .unwrap();
    let url = client.authorize_url("http://127.0.0.1:49152", "the-state", "the-challenge");
    assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
    let query = query_of(&url);
    let expected: HashMap<String, String> = [
        ("client_id", CLIENT),
        ("response_type", "code"),
        ("redirect_uri", "http://127.0.0.1:49152"),
        ("scope", "https://mail.google.com/ openid email"),
        ("state", "the-state"),
        ("code_challenge", "the-challenge"),
        ("code_challenge_method", "S256"),
        ("access_type", "offline"),
        ("prompt", "consent select_account"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value.to_owned()))
    .collect();
    assert_eq!(query, expected);
    assert!(
        !url.contains(SECRET),
        "the secret never goes to the browser"
    );
    assert!(!url.contains("login_hint"));
}

#[tokio::test]
async fn a_google_sign_in_redirects_to_127_0_0_1_on_a_port_of_its_own() {
    let google = FakeGoogle::start().await;
    let client = google.client();
    let first = client.begin().await.unwrap();
    let second = client.begin().await.unwrap();
    let (first, second) = (query_of(first.url()), query_of(second.url()));
    for query in [&first, &second] {
        let redirect = &query["redirect_uri"];
        let port: u16 = redirect
            .strip_prefix("http://127.0.0.1:")
            .expect("Google's loopback IP form")
            .parse()
            .unwrap();
        assert_ne!(port, 0);
        assert!(TcpStream::connect(("127.0.0.1", port)).await.is_ok());
        assert_eq!(query["code_challenge"].len(), 43);
    }
    assert_ne!(
        first["redirect_uri"], second["redirect_uri"],
        "ephemeral ports"
    );
    assert_ne!(first["state"], second["state"]);
    assert_ne!(first["code_challenge"], second["code_challenge"]);
}

#[test]
fn googles_redirect_is_read_for_its_code_or_error() {
    let code = callback(
        "/?state=s-1&code=4/0AeanS0abc&scope=email%20https://mail.google.com/%20openid",
        "s-1",
    );
    assert!(matches!(code, Some(Callback::Code(code)) if code.as_str() == "4/0AeanS0abc"));
    match callback("/?error=access_denied&state=s-1", "s-1") {
        Some(Callback::Error { error, code }) => {
            assert_eq!(error, "access_denied");
            assert_eq!(code, None);
            let error = authorize_error(&error, code);
            assert!(matches!(error, OAuthError::Declined(None)), "{error}");
            assert_eq!(error.failure(), LoginFailure::SignInDeclined);
        }
        _ => panic!("the error wasn't read"),
    }
    assert!(
        callback("/?state=other&code=x", "s-1").is_none(),
        "another state"
    );
    assert!(callback("/?code=x", "s-1").is_none(), "no state");
    assert!(callback("/favicon.ico?state=s-1&code=x", "s-1").is_none());
    assert!(
        callback("/?state=s-1", "s-1").is_none(),
        "neither code nor error"
    );
}

#[test]
fn googles_id_token_names_the_verified_address() {
    let at = SystemTime::now();
    let identity = check(
        OAuthProvider::Google,
        &jwt(&google_claims("one@gmail.com")),
        CLIENT,
        at,
    )
    .unwrap();
    assert_eq!(identity.username, "one@gmail.com");
    assert!(!identity.consumer());
    assert!(!format!("{identity:?}").contains("gmail"));

    let with = |change: &dyn Fn(&mut serde_json::Value)| {
        let mut claims = google_claims("one@gmail.com");
        change(&mut claims);
        check(OAuthProvider::Google, &jwt(&claims), CLIENT, at)
    };
    assert!(with(&|c| c["iss"] = "accounts.google.com".into()).is_ok());
    assert!(with(&|c| c["email_verified"] = "true".into()).is_ok());
    for (what, change) in [
        (
            "another issuer",
            &(|c: &mut serde_json::Value| {
                c["iss"] = "https://login.microsoftonline.com/x/v2.0".into()
            }) as &dyn Fn(&mut serde_json::Value),
        ),
        ("another client", &|c| {
            c["aud"] = "other.apps.googleusercontent.com".into()
        }),
        ("unverified", &|c| c["email_verified"] = false.into()),
        ("no verification", &|c| {
            c.as_object_mut().unwrap().remove("email_verified");
        }),
        ("no address", &|c| {
            c.as_object_mut().unwrap().remove("email");
        }),
        ("expired", &|c| c["exp"] = (now() - 3600).into()),
    ] {
        assert!(
            matches!(with(change), Err(OAuthError::IdToken(_))),
            "{what} must be refused"
        );
    }
    // A Microsoft check never takes Google's token, nor the other way round.
    assert!(
        check(
            OAuthProvider::Microsoft,
            &jwt(&google_claims("one@gmail.com")),
            CLIENT,
            at
        )
        .is_err()
    );
}

#[tokio::test]
async fn the_code_exchange_and_refreshes_send_the_client_secret() {
    let google = FakeGoogle::start().await;
    google.state().next_user = ACCOUNTS[0].into();
    let client = google.client();
    let tokens = client
        .exchange("4/code", "http://127.0.0.1:5000", "verifier")
        .await
        .unwrap();
    assert!(tokens.refresh.is_some());
    assert!(tokens.granted(GOOGLE_MAIL_SCOPE));
    let refreshed = client
        .refresh(tokens.refresh.as_deref().unwrap())
        .await
        .unwrap();
    assert!(
        refreshed.refresh.is_none(),
        "Google keeps its refresh token"
    );
    assert_eq!(refreshed.lifetime, Duration::from_secs(3599));
    let forms = google.state().forms.clone();
    assert_eq!(forms.len(), 2);
    assert_eq!(forms[0]["grant_type"], "authorization_code");
    assert_eq!(forms[0]["code_verifier"], "verifier");
    assert_eq!(forms[0]["redirect_uri"], "http://127.0.0.1:5000");
    assert_eq!(forms[1]["grant_type"], "refresh_token");
    assert!(!forms[1].contains_key("scope"));
    assert!(forms.iter().all(|form| form["client_secret"] == SECRET));

    // Without the secret Google refuses the client, which isn't a sign-in to do again.
    let bare = Client::new(CLIENT.into(), google.provider()).unwrap();
    let error = bare.refresh("1//anything").await.unwrap_err();
    assert!(matches!(error, OAuthError::Refused(None)), "{error}");
    assert_ne!(error.failure(), LoginFailure::OAuthRequired);
    assert!(!google.state().forms[2].contains_key("client_secret"));

    // A revoked refresh token means signing in again.
    let error = client.refresh("1//revoked").await.unwrap_err();
    assert!(matches!(error, OAuthError::RefreshRefused(None)), "{error}");
    assert_eq!(error.failure(), LoginFailure::OAuthRequired);
    assert!(!error.to_string().contains("revoked"), "no server text");
}

#[test]
fn the_client_secret_is_never_shown() {
    let registration = Registration::new(Some(" id.apps.googleusercontent.com "), Some(SECRET));
    let registration = registration.unwrap();
    assert_eq!(registration.id, "id.apps.googleusercontent.com");
    let shown = format!("{registration:?}");
    assert!(
        !shown.contains(SECRET) && !shown.contains("googleusercontent"),
        "{shown}"
    );
    let client = Client::registered(&registration, Provider::google()).unwrap();
    let shown = format!("{client:?}");
    assert!(!shown.contains(SECRET), "{shown}");
    assert_eq!(client.provider(), OAuthProvider::Google);
    assert_eq!(Registration::new(Some("  "), Some(SECRET)), None, "no ID");
    assert_eq!(Registration::new(None, Some(SECRET)), None);
    assert_eq!(
        Registration::new(Some("id"), Some(" ")).unwrap().secret,
        None,
        "a blank secret is none"
    );
}

#[test]
fn gmails_xoauth2_string_is_the_address_and_the_bearer_token() {
    // Google's documented example (developers.google.com/gmail/imap/xoauth2-protocol).
    let response = crate::imap::xoauth2_response(
        "someuser@example.com",
        "ya29.vF9dft4qmTc2Nvb3RlckBhdHRhdmlzdGEuY29tCg",
    );
    assert_eq!(
        STANDARD.encode(&*response),
        "dXNlcj1zb21ldXNlckBleGFtcGxlLmNvbQFhdXRoPUJlYXJlciB5YTI5LnZGOWRmdDRxbVRjMk52\
         YjNSbGNrQmhkSFJoZG1semRHRXVZMjl0Q2cBAQ=="
    );
    for user in ACCOUNTS {
        let response = crate::imap::xoauth2_response(user, "ya29.token");
        assert_eq!(
            &*response,
            format!("user={user}\x01auth=Bearer ya29.token\x01\x01").as_bytes()
        );
    }
}

async fn stored(secrets: &MemoryStore, key: &AccountKey) -> String {
    let secret = secrets
        .read(&key.0, SecretKind::RefreshToken)
        .await
        .unwrap()
        .unwrap();
    String::from_utf8(secret.to_vec()).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn three_gmail_accounts_keep_their_own_tokens() {
    let google = FakeGoogle::start().await;
    let client = google.client();
    let secrets = MemoryStore::default();
    let tokens = AccessTokens::default();
    let keys: Vec<AccountKey> = (1..=3).map(|n| AccountKey(format!("gmail-{n}"))).collect();
    let mut granted = Vec::new();
    for (key, user) in keys.iter().zip(ACCOUNTS) {
        let refresh = google.grant(user);
        secrets
            .store(&key.0, SecretKind::RefreshToken, user, refresh.as_bytes())
            .await
            .unwrap();
        granted.push(refresh);
    }
    let get = |n: usize, renew| tokens.get(&keys[n], ACCOUNTS[n], &secrets, Some(&client), renew);
    let owner = |token: &str| google.state().access.get(token).cloned();

    // All three at once: each refreshes with its own refresh token and gets its own token.
    let (a, b, c) = tokio::join!(get(0, false), get(1, false), get(2, false));
    let fetched = [a.unwrap(), b.unwrap(), c.unwrap()];
    for (n, fetched) in fetched.iter().enumerate() {
        assert!(fetched.refreshed);
        assert_eq!(owner(&fetched.token).as_deref(), Some(ACCOUNTS[n]));
        assert_eq!(
            stored(&secrets, &keys[n]).await,
            granted[n],
            "kept, not rotated"
        );
    }
    let used: Vec<String> = google
        .state()
        .forms
        .iter()
        .map(|form| form["refresh_token"].clone())
        .collect();
    assert_eq!(used.len(), 3);
    assert_eq!(
        used.iter().collect::<HashSet<_>>(),
        granted.iter().collect::<HashSet<_>>(),
        "each refresh token used once, by its own account"
    );

    // Renewing one leaves the others' cached tokens and stored refresh tokens alone.
    let renewed = get(1, true).await.unwrap();
    assert_ne!(renewed.token, fetched[1].token);
    assert_eq!(owner(&renewed.token).as_deref(), Some(ACCOUNTS[1]));
    for n in [0, 2] {
        let cached = get(n, false).await.unwrap();
        assert!(!cached.refreshed);
        assert_eq!(cached.token, fetched[n].token);
    }
    assert_eq!(google.state().forms.len(), 4, "one more refresh only");

    // One account revoked: only it must sign in again; the others refresh as before.
    google.state().refresh.remove(&granted[0]);
    assert_eq!(get(0, true).await.err(), Some(LoginFailure::OAuthRequired));
    assert_eq!(stored(&secrets, &keys[0]).await, granted[0]);
    for n in [1, 2] {
        let fetched = get(n, true).await.unwrap();
        assert_eq!(owner(&fetched.token).as_deref(), Some(ACCOUNTS[n]));
    }

    // A re-sign-in of one account replaces only its refresh token.
    let again = google.grant(ACCOUNTS[0]);
    secrets
        .store(
            &keys[0].0,
            SecretKind::RefreshToken,
            ACCOUNTS[0],
            again.as_bytes(),
        )
        .await
        .unwrap();
    let fetched = get(0, false).await.unwrap();
    assert_eq!(owner(&fetched.token).as_deref(), Some(ACCOUNTS[0]));
    assert_eq!(stored(&secrets, &keys[1]).await, granted[1]);
    assert_eq!(stored(&secrets, &keys[2]).await, granted[2]);

    // A removed account's token goes; the others stay cached.
    tokens.forget(&keys[2]);
    secrets
        .delete(&keys[2].0, SecretKind::RefreshToken)
        .await
        .unwrap();
    assert_eq!(get(2, false).await.err(), Some(LoginFailure::OAuthRequired));
    assert!(!get(1, false).await.unwrap().refreshed);
}
