//! Helpers for Kite Connect's interactive login flow.

use std::collections::HashMap;

use nautilus_core::hex;
use nautilus_core::string::secret::SecretString;
use nautilus_network::http::{HttpClient, HttpMethod, Url};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::common::consts::{KITE_VERSION, KITE_VERSION_HEADER};
use crate::credential::{KiteAccessToken, KiteApiKey, KiteApiSecret, KiteRequestToken};
use crate::http::consts::{CONTENT_TYPE_HEADER, FORM_CONTENT_TYPE, TIMEOUT_SECONDS};
use crate::http::error::KiteHttpError;
use crate::http::response::{decode, ensure_success};
use crate::urls::{api_url, join_login_url};

const CONNECT_LOGIN_PATH: &str = "/connect/login";
const TOKEN_EXCHANGE_PATH: &str = "/session/token";

/// Returns the URL where a user begins Kite Connect's [login flow].
///
/// [login flow]: https://kite.trade/docs/connect/v3/user/#login-flow
pub fn login_url(api_key: &KiteApiKey) -> Url {
    let mut url = join_login_url(CONNECT_LOGIN_PATH);
    url.query_pairs_mut()
        .append_pair("api_key", api_key.expose_secret())
        .append_pair("v", KITE_VERSION);
    url
}

/// Exchanges the request token from Kite's login redirect for an access token.
///
/// This request is not retried because request tokens are short-lived and single-use.
///
/// # Errors
///
/// Returns an error if the request cannot be sent or Kite rejects the token exchange.
pub async fn exchange_request_token(
    api_key: &KiteApiKey,
    api_secret: &KiteApiSecret,
    request_token: &KiteRequestToken,
) -> Result<KiteAccessToken, KiteHttpError> {
    let checksum = checksum(api_key, api_secret, request_token);
    let body = serde_urlencoded::to_string([
        ("api_key", api_key.expose_secret()),
        ("request_token", request_token.expose_secret()),
        ("checksum", checksum.as_str()),
    ])?;
    let headers = HashMap::from([
        (KITE_VERSION_HEADER.to_owned(), KITE_VERSION.to_owned()),
        (CONTENT_TYPE_HEADER.to_owned(), FORM_CONTENT_TYPE.to_owned()),
    ]);
    let client = HttpClient::builder()
        .headers(headers)
        .redact_url(true)
        .timeout_secs(TIMEOUT_SECONDS)
        .build()?;
    let response = client
        .request_with_secret_body(
            HttpMethod::POST.into(),
            api_url(TOKEN_EXCHANGE_PATH).to_string(),
            None,
            None,
            SecretString::from(body),
            None,
            None,
        )
        .await?;
    let response = ensure_success(response)?;
    let response: KiteAccessTokenResponse = decode(response)?;

    Ok(response.access_token)
}

#[derive(Debug, Deserialize)]
struct KiteAccessTokenResponse {
    access_token: KiteAccessToken,
}

fn checksum(
    api_key: &KiteApiKey,
    api_secret: &KiteApiSecret,
    request_token: &KiteRequestToken,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(api_key.expose_secret());
    hasher.update(request_token.expose_secret());
    hasher.update(api_secret.expose_secret());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use nautilus_core::string::secret::SecretString;

    use crate::credential::{KiteApiKey, KiteApiSecret, KiteRequestToken};
    use crate::login::{checksum, login_url};

    #[test]
    fn builds_login_url() {
        let api_key = KiteApiKey::new(SecretString::from("api key/+"));

        assert_eq!(
            login_url(&api_key).as_str(),
            "https://kite.zerodha.com/connect/login?api_key=api+key%2F%2B&v=3"
        );
    }

    #[test]
    fn generates_kite_login_checksum() {
        let api_key = KiteApiKey::new(SecretString::from("api-key"));
        let api_secret = KiteApiSecret::new(SecretString::from("api-secret"));
        let request_token = KiteRequestToken::new(SecretString::from("request-token"));

        assert_eq!(
            checksum(&api_key, &api_secret, &request_token),
            "d93f7cb933c3518b3a5f87fa0b49ff6bc71de987dc59bc8015b296920b762fd0"
        );
    }
}
