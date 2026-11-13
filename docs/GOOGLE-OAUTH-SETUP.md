# OAuth setup: Sign in with Google (Gmail)

Synced: 2026-10-22

Sign in with Google lets Pronto reach Gmail over IMAP and SMTP with a browser sign-in instead of an app password ([ADR-023](DECISIONS.md#adr-023--sign-in-with-google-with-your-own-oauth-client-2026-10-22)). Pronto ships **no** Google client: Google's `https://mail.google.com/` scope is *restricted*, and a client that anyone may use needs Google's verification and a yearly security assessment. Instead, you make your own OAuth client in the Google Cloud console, for your own accounts, and give Pronto its ID and secret. This page shows how, then how to add your Gmail accounts.

Without a Google client, nothing changes: Gmail keeps working with an app password ([GMAIL.md](GMAIL.md)).

- [What you get, and the catch](#what-you-get-and-the-catch)
- [Step 1: A Google Cloud project](#step-1-a-google-cloud-project)
- [Step 2: The consent screen (Google Auth Platform)](#step-2-the-consent-screen-google-auth-platform)
- [Step 3: Test users](#step-3-test-users)
- [Step 4: The scope](#step-4-the-scope)
- [Step 5: A Desktop app client](#step-5-a-desktop-app-client)
- [Step 6: Give Pronto the client](#step-6-give-pronto-the-client)
- [Step 7: Add your Gmail accounts](#step-7-add-your-gmail-accounts)
- [Several Gmail accounts](#several-gmail-accounts)
- [Moving an app-password account to Sign in with Google](#moving-an-app-password-account-to-sign-in-with-google)
- [If something goes wrong](#if-something-goes-wrong)
- [What Pronto does with it](#what-pronto-does-with-it)

Google's consoles change; the steps below follow Google's documentation as read on 2026-10-22 ("OAuth 2.0 for iOS & Desktop Apps", "Configure the OAuth consent screen", "Unverified apps"). If a screen differs, the names of the settings (Desktop app, External, Test users, the scope) are what matter.

## What you get, and the catch

- **Gmail and Google Workspace** accounts sign in in your browser; Pronto never sees your Google password, and you don't need 2-Step Verification or an app password.
- **Your client, your accounts.** The client is yours: only the Google accounts you list as test users (step 3) can sign in through it while it is in *Testing*.
- **The catch: in Testing, Google ends the sign-in after 7 days.** Google's documentation says refresh tokens of an app in *Testing* that asks for more than name, email and profile expire after seven days. Pronto then shows the account's banner, "needs you to sign in again", and one click on **Sign in** fixes it until the next week. To avoid that, see [Testing or In production](#testing-or-in-production) in step 3.

## Step 1: A Google Cloud project

1. Open [console.cloud.google.com](https://console.cloud.google.com/) and sign in with any Google account you own (it doesn't have to be one of the mail accounts).
2. Open the project picker at the top and choose **New project**. Name it `Pronto` (any name works), and click **Create**. Make sure the new project is selected in the picker afterwards.
3. Optional but handy: **APIs & Services → Library**, search **Gmail API**, and click **Enable**. IMAP and SMTP don't need the Gmail API, but with it enabled the console's scope picker in step 4 lists Gmail's scopes, so you can tick the right one instead of typing it.

## Step 2: The consent screen (Google Auth Platform)

1. Open **Google Auth Platform** (in the console's menu, or **APIs & Services → OAuth consent screen**, which leads there). If it says Google Auth Platform isn't configured yet, click **Get started**.
2. **App information:** App name `Pronto` (this is what Google's sign-in page shows), and your own address as **User support email**.
3. **Audience:** choose **External**. (*Internal* exists only for a Google Workspace organization and limits sign-in to its own users; choose it if all your mail accounts are in that one Workspace organization.)
4. **Contact information:** your address again.
5. Agree to Google's API Services User Data Policy, and click **Create**.

## Step 3: Test users

1. In Google Auth Platform, open **Audience**.
2. Under **Test users**, click **Add users** and add **every Gmail address you will add to Pronto**, for example all three of yours. Click **Save**. While the app is in *Testing*, Google lets only these accounts sign in; anyone else sees "Error 403: access_denied".

### Testing or In production

The **Publishing status** on the same page is **Testing** at first. For your own accounts you have two choices:

- **Stay in Testing** (the default): only your test users can sign in, and Google asks each of them to sign in again every 7 days (above).
- **Publish to In production** (**Publish app**): Google doesn't verify the app for you, so its sign-in page warns "Google hasn't verified this app"; you continue with **Advanced → Go to Pronto (unsafe)**. Google's documentation allows an unverified app for personal use and caps it at 100 users; the 7-day expiry of Testing doesn't apply. Anyone who learns your client ID could then *start* a sign-in with it, but they would only ever reach their own mailbox, and the warning page tells them the app isn't verified. Don't publish the client ID or secret.

Pronto works the same either way. Choose Testing if you don't mind a weekly sign-in, In production if you do.

## Step 4: The scope

1. In Google Auth Platform, open **Data Access** and click **Add or remove scopes**.
2. Tick, or add under **Manually add scopes**, this one:
   - `https://mail.google.com/` — "Read, compose, send, and permanently delete all your email from Gmail". Gmail's IMAP and SMTP accept only this scope with XOAUTH2; Google lists it as a *restricted* scope.
3. `openid` and `email` (`.../auth/userinfo.email`) are basic scopes Pronto also asks for, to learn which address you signed in as; you can add them here too, but you don't have to.
4. Click **Update**, then **Save**.

## Step 5: A Desktop app client

1. In Google Auth Platform, open **Clients** and click **Create client**.
2. **Application type: Desktop app.** (Not *Web application*: a Desktop app client accepts Pronto's redirect, `http://127.0.0.1:<port>`, on whichever free port Pronto picks, without registering it.)
3. Name it `Pronto on my laptop` (any name), and click **Create**.
4. Google shows the **Client ID** (ending in `.apps.googleusercontent.com`) and the **Client secret** (starting with `GOCSPX-`). Copy both, or click **Download JSON**. You can see the ID again later on the Clients page; recent consoles show the secret only once, so keep the JSON somewhere safe (not in a repository).

Google's own documentation for installed apps says such a client secret can't be kept confidential and isn't treated as one; it still identifies *your* client, so keep it to yourself.

## Step 6: Give Pronto the client

Pronto looks for the Google client in this order, and uses the first it finds:

1. **`config.toml`** (recommended). Open it with **Preferences** in Pronto's menu (or `~/.config/pronto/config.toml`) and add:

   ```toml
   [oauth.google]
   client_id = "1234567890-abcdefg.apps.googleusercontent.com"
   client_secret = "GOCSPX-xxxxxxxxxxxxxxxxxxxxxxxx"
   ```

   Pronto applies it as soon as you save the file; no restart. Make the file readable only by you: `chmod 600 ~/.config/pronto/config.toml`.
2. **The environment Pronto runs in:** `PRONTO_GOOGLE_CLIENT_ID` and `PRONTO_GOOGLE_CLIENT_SECRET`. On Hyprland, Pronto started from the launcher gets them from `~/.config/hypr/hyprland.conf` (or a file it sources, such as Omarchy's `~/.config/hypr/envs.conf`):

   ```
   env = PRONTO_GOOGLE_CLIENT_ID,1234567890-abcdefg.apps.googleusercontent.com
   env = PRONTO_GOOGLE_CLIENT_SECRET,GOCSPX-xxxxxxxxxxxxxxxxxxxxxxxx
   ```

   Hyprland reads `env` lines when it starts, so log out and in again after adding them. They're read when Pronto starts.
3. **Built in:** the same two variables exported before `cargo build` are compiled into that build (`option_env!`). Only do this for a build you keep to yourself.

The ID and the secret never go into the repository, `accounts.toml` or the log; Pronto sends the secret only to Google's token endpoint over HTTPS. A `[oauth.google]` section with a `client_id` wins over the environment entirely: its own `client_secret` (or none) is used, never the environment's. Removing the section (or blanking its `client_id`) goes back to the environment's client.

## Step 7: Add your Gmail accounts

1. In Pronto, click **Add an account** (or **Add Account** in the main menu).
2. Type your Gmail address and your name, and click **Next**.
3. Pronto finds Gmail's servers and, because a Google client is set, shows **Sign in with Google**. (Without a client it asks for an app password instead, as before.) Click it.
4. Your browser opens Google's sign-in page. **Choose the account you typed** in Google's account chooser (it lists every Google account signed in to that browser), sign in if asked, and allow Pronto access to your Gmail. In Testing or with an unverified app, Google first warns that it hasn't verified the app: **Continue** (or **Advanced → Go to Pronto (unsafe)**).
5. The browser tab says "Pronto has the sign-in. You can close this tab and return to Pronto." Pronto checks IMAP and SMTP with the new sign-in and adds the account.

If you picked another account in Google's chooser than the address you typed, Pronto says so and asks which address to send from; usually you want **Back** and to sign in again with the right one.

## Several Gmail accounts

Add each account the same way, one after the other; the same Google client serves all of them. Each account keeps its own sign-in: its refresh token is stored in your keyring under that account's own key (the `key` in `accounts.toml`), its access tokens stay in memory for that account only, and a refresh, a revoked sign-in or a sign-in made again touches only that account. If one account must sign in again (for example after 7 days in Testing, or because you revoked Pronto in that Google account), only that account's banner shows; the others keep syncing.

Google's account chooser opens with whichever account the browser used last, so check the address before you allow access.

## Moving an app-password account to Sign in with Google

Either remove the account (**Remove Account**) and add it again as above, which downloads its mail again, or keep its mail and switch it in place (not yet tried on a real account):

1. Quit Pronto. In `~/.config/pronto/accounts.toml`, in that account's `[[account]]`, set `auth = "google"` (and make sure `username` is the Gmail address, or leave the line out).
2. Start Pronto. The account can't sync yet and its banner says it "needs you to sign in again". Click **Sign in**, and sign in with that same Google account.

The old app password stays in the keyring until the account is removed; delete it in Google too, at [myaccount.google.com/apppasswords](https://myaccount.google.com/apppasswords).

## If something goes wrong

- **The assistant still asks for an app password for Gmail:** no Google client is in use. Check `[oauth.google]` in `config.toml` (Pronto shows a mistake in it with its line), or that the environment variables reached Pronto.
- **Google says "Error 403: access_denied" and "has not completed the Google verification process":** the account isn't a test user (step 3) while the app is in Testing.
- **Google says "Error 400: redirect_uri_mismatch" or "invalid_request":** the client isn't a **Desktop app** client (step 5). Make a Desktop app client and use its ID and secret.
- **Pronto says "The sign-in didn't work" right after the browser came back:** usually a wrong or missing client secret (Google answers `invalid_client`). Copy the secret again.
- **"Google didn't let Pronto in":** you clicked Cancel or didn't allow access on Google's page, or a Workspace admin blocks the app. Sign in again and allow it.
- **"The server refused this sign-in":** the scope `https://mail.google.com/` wasn't granted; check step 4, or a Workspace admin turned off IMAP or third-party access.
- **Every 7 days an account "needs you to sign in again":** the app is in Testing (step 3).

Leave your address, client ID and secret out of any issue you open.

## What Pronto does with it

- **The flow:** OAuth 2.0 authorization code with PKCE (S256), in your system browser, with a loopback redirect `http://127.0.0.1:<free port>` that Pronto listens on only during the sign-in and that ignores any request without the sign-in's random state. It asks for `https://mail.google.com/ openid email`, with `access_type=offline` (for a refresh token) and `prompt=consent select_account`. Your address is never put in the sign-in page's address.
- **Who you are** comes from the ID token Google returns with the tokens: its verified `email` becomes the account's username, which is what Gmail's XOAUTH2 needs.
- **Tokens:** the refresh token goes only to the keyring, per account; access tokens (one hour) stay in memory and are refreshed 5 minutes before they run out, or once when Gmail refuses one. Google keeps the refresh token across refreshes; if Google refuses it (revoked, expired, password changed), the account asks you to sign in again.
- **IMAP and SMTP** log in with SASL XOAUTH2 at `imap.gmail.com:993` and `smtp.gmail.com:465`, both TLS.
- **Stop Pronto's access** at any time in each Google account, at [myaccount.google.com/permissions](https://myaccount.google.com/permissions) (**Third-party apps & services → Pronto → Delete all connections**). Your mail stays in Gmail.
