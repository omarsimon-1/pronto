//! A separate, cancellable connection for bounded style sampling. No body cache is involved.

use super::{Shared, log_in_with_password, log_in_with_token};
use crate::api::{BodyError, Event, RequestId};
use crate::sent_sample::{Cancel, SampleOptions, SentSample};
use sealwax_domain::account::{AuthMethod, Receive};
use sealwax_domain::model::AccountId;
use std::sync::Arc;

pub(super) async fn run(
    shared: Arc<Shared>,
    request: RequestId,
    account: AccountId,
    options: SampleOptions,
    cancel: Cancel,
) {
    let result = tokio::select! {
        biased;
        () = cancel.cancelled() => Err(BodyError::Other),
        result = sample(&shared, request, account, options) => result,
    };
    shared.send(Event::SentSampled {
        request,
        account,
        result,
    });
}

async fn sample(
    shared: &Shared,
    request: RequestId,
    account: AccountId,
    options: SampleOptions,
) -> Result<SentSample, BodyError> {
    let settings = shared.actor_settings(account).ok_or(BodyError::Other)?;
    if !matches!(settings.receive, Receive::Imap) {
        return Err(BodyError::Other);
    }
    let mut conn = match settings.auth {
        AuthMethod::Password => log_in_with_password(shared, &settings).await,
        AuthMethod::OAuth(_) => log_in_with_token(shared, &settings).await,
    }
    .map_err(
        |failure| match crate::api::SyncStatus::from_failure(failure) {
            crate::api::SyncStatus::NeedsSignIn(_) => BodyError::NeedsSignIn,
            _ => BodyError::Offline,
        },
    )?;
    let result = crate::sent_sample::sample(&mut conn, &options, |fetched, total| {
        shared.send(Event::SentSampleProgress {
            request,
            account,
            fetched,
            total,
        });
    })
    .await
    .map_err(|error| {
        if error.is_connection() {
            BodyError::Offline
        } else {
            BodyError::Other
        }
    });
    conn.logout().await;
    result
}
