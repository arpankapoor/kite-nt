use nautilus_core::string::secret::SecretString;
use serde::{Deserialize, Serialize};

/// Identifies a Kite Connect application in the [login flow].
///
/// [login flow]: https://kite.trade/docs/connect/v3/user/#login-flow
#[derive(Debug)]
pub struct KiteApiKey(SecretString);

/// Secret issued with a Kite Connect API key.
#[derive(Debug)]
pub struct KiteApiSecret(SecretString);

/// Short-lived token returned by Kite's login redirect.
#[derive(Debug)]
pub struct KiteRequestToken(SecretString);

/// Authenticates Kite Connect requests for a user session.
///
/// Access tokens are obtained through Kite's [login flow].
///
/// [login flow]: https://kite.trade/docs/connect/v3/user/#login-flow
#[derive(Debug, Deserialize, Serialize)]
pub struct KiteAccessToken(SecretString);

/// Credentials for an already authenticated Kite Connect session.
#[derive(Debug)]
pub struct KiteSessionCredentials {
    api_key: KiteApiKey,
    access_token: KiteAccessToken,
}

impl KiteApiKey {
    /// Creates an API key from secret text.
    pub fn new(value: SecretString) -> Self {
        Self(value)
    }

    pub(crate) fn expose_secret(&self) -> &str {
        self.0.expose_secret()
    }
}

impl KiteApiSecret {
    /// Creates an API secret from secret text.
    pub fn new(value: SecretString) -> Self {
        Self(value)
    }

    pub(crate) fn expose_secret(&self) -> &str {
        self.0.expose_secret()
    }
}

impl KiteRequestToken {
    /// Creates a request token from secret text.
    pub fn new(value: SecretString) -> Self {
        Self(value)
    }

    pub(crate) fn expose_secret(&self) -> &str {
        self.0.expose_secret()
    }
}

impl KiteAccessToken {
    /// Creates an access token from secret text.
    pub fn new(value: SecretString) -> Self {
        Self(value)
    }

    pub(crate) fn expose_secret(&self) -> &str {
        self.0.expose_secret()
    }
}

impl KiteSessionCredentials {
    /// Creates credentials for an authenticated Kite Connect session.
    pub fn new(api_key: KiteApiKey, access_token: KiteAccessToken) -> Self {
        Self {
            api_key,
            access_token,
        }
    }

    /// Returns the Kite Connect API key.
    pub fn api_key(&self) -> &KiteApiKey {
        &self.api_key
    }

    /// Returns the session access token.
    pub fn access_token(&self) -> &KiteAccessToken {
        &self.access_token
    }
}
