//! This crate provides a library for authenticating with the GitHub
//! API as a GitHub app. See
//! [Authenticating with GitHub Apps](https://developer.github.com/apps/building-github-apps/authenticating-with-github-apps)
//! for details about the authentication flow.
//!
//! Example:
//!
//! ```no_run
//! use github_app_auth::{GithubAuthParams, InstallationToken};
//!
//! // The token is mutable because the installation token must be
//! // periodically refreshed. See the `GithubAuthParams` documentation
//! // for details on how to get the private key and the two IDs.
//! let mut token = InstallationToken::new(GithubAuthParams {
//!     user_agent: "my-cool-user-agent".into(),
//!     private_key: b"my private key".to_vec(),
//!     app_id: 1234,
//!     installation_id: 5678,
//! }).expect("failed to get installation token");
//!
//! // Getting the authentication header will automatically refresh
//! // the token if necessary, but of course this operation can fail.
//! let header = token.header().expect("failed to get authentication header");
//!
//! token.client.post("https://some-github-api-url").headers(header).send();
//! ```

use chrono::{DateTime, Utc};
use log::info;
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use std::time;

const MACHINE_MAN_PREVIEW: &str =
    "application/vnd.github.machine-man-preview+json";

/// Authentication error enum.
#[derive(thiserror::Error, Debug)]
pub enum AuthError {
    /// An error occurred when trying to encode the JWT.
    #[error("JWT encoding failed")]
    JwtError(#[from] jsonwebtoken::errors::Error),

    /// The token cannot be encoded as an HTTP header.
    #[error("HTTP header encoding failed")]
    InvalidHeaderValue(#[from] http::header::InvalidHeaderValue),

    /// An HTTP request failed.
    #[error("HTTP request failed")]
    ReqwestError(#[from] reqwest::Error),

    /// Something very unexpected happened with time itself.
    #[error("system time error")]
    TimeError(#[from] time::SystemTimeError),
}

#[derive(Debug, Serialize)]
struct JwtClaims {
    /// The time that this JWT was issued
    iat: u64,
    // JWT expiration time
    exp: u64,
    // GitHub App's identifier number
    iss: u64,
}

impl JwtClaims {
    fn new(params: &GithubAuthParams) -> Result<JwtClaims, AuthError> {
        let now = time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)?
            .as_secs();
        Ok(JwtClaims {
            // The time that this JWT was issued (now)
            iat: now,
            // JWT expiration time (1 minute from now)
            exp: now + 60,
            // GitHub App's identifier number
            iss: params.app_id,
        })
    }
}

/// This is the structure of the JSON object returned when requesting
/// an installation token.
#[derive(Debug, Deserialize, Eq, PartialEq)]
struct RawInstallationToken {
    token: String,
    expires_at: DateTime<Utc>,
}

/// Use the app private key to generate a JWT and use the JWT to get
/// an installation token.
///
/// Reference:
/// developer.github.com/apps/building-github-apps/authenticating-with-github-apps
fn get_installation_token(
    client: &reqwest::blocking::Client,
    params: &GithubAuthParams,
) -> Result<RawInstallationToken, AuthError> {
    let claims = JwtClaims::new(params)?;
    let mut header = jsonwebtoken::Header::default();
    header.alg = jsonwebtoken::Algorithm::RS256;
    let private_key =
        jsonwebtoken::EncodingKey::from_secret(&params.private_key);
    let token = jsonwebtoken::encode(&header, &claims, &private_key)?;

    let url = format!(
        "https://api.github.com/app/installations/{}/access_tokens",
        params.installation_id
    );
    Ok(client
        .post(&url)
        .bearer_auth(token)
        .header("Accept", MACHINE_MAN_PREVIEW)
        .send()?
        .error_for_status()?
        .json()?)
}

/// An installation token is the primary method for authenticating
/// with the GitHub API as an application.
pub struct InstallationToken {
    /// The `reqwest::blocking::Client` used to periodically refresh the token.
    ///
    /// This is made public so that users of the library can re-use
    /// this client for sending requests, but this is not required.
    pub client: reqwest::blocking::Client,

    token: String,
    fetch_time: time::SystemTime,
    params: GithubAuthParams,
}

impl InstallationToken {
    /// Fetch an installation token using the provided authentication
    /// parameters.
    pub fn new(
        params: GithubAuthParams,
    ) -> Result<InstallationToken, AuthError> {
        let client = reqwest::blocking::Client::builder()
            .user_agent(&params.user_agent)
            .build()?;
        let raw = get_installation_token(&client, &params)?;
        Ok(InstallationToken {
            client,
            token: raw.token,
            fetch_time: time::SystemTime::now(),
            params,
        })
    }

    /// Get an HTTP authentication header for the installation token.
    ///
    /// This method is mutable because the installation token must be
    /// periodically refreshed.
    pub fn header(&mut self) -> Result<HeaderMap, AuthError> {
        self.refresh()?;
        let mut headers = HeaderMap::new();
        let val = format!("token {}", self.token);
        headers.insert("Authorization", val.parse()?);
        Ok(headers)
    }

    fn refresh(&mut self) -> Result<(), AuthError> {
        let elapsed =
            time::SystemTime::now().duration_since(self.fetch_time)?;
        // Installation tokens expire after 60 minutes. Refresh them
        // after 55 minutes to give ourselves a little wiggle room.
        if elapsed.as_secs() > (55 * 60) {
            info!("refreshing installation token");
            let raw = get_installation_token(&self.client, &self.params)?;
            self.token = raw.token;
            self.fetch_time = time::SystemTime::now();
        }
        Ok(())
    }
}

/// Input parameters for authenticating as a GitHub app. This is used
/// to get an installation token.
#[derive(Clone)]
pub struct GithubAuthParams {
    /// User agent set for all requests to GitHub. The API requires
    /// that a user agent is set:
    /// https://developer.github.com/v3/#user-agent-required
    ///
    /// They "request that you use your GitHub username, or the name
    /// of your application".
    pub user_agent: String,

    /// Private key used to sign access token requests. You can
    /// generate a private key at the bottom of the application's
    /// settings page.
    pub private_key: Vec<u8>,

    /// GitHub application installation ID. To find this value you can
    /// look at the app installation's configuration URL.
    ///
    /// - For organizations this is on the "Installed GitHub Apps"
    ///   page in your organization's settings page.
    ///
    /// - For personal accounts, go to the "Applications" page and
    ///   select the "Installed GitHub Apps" tab.
    ///
    /// The installation ID will be the final component of the path,
    /// for example "1216616" is the installation ID for
    /// "github.com/organizations/mycoolorg/settings/installations/1216616".
    pub installation_id: u64,

    /// GitHub application ID. You can find this in the application
    /// settings page on GitHub under "App ID".
    pub app_id: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn test_raw_installation_token_parse() {
        let resp = r#"{
            "token": "v1.1f699f1069f60xxx",
            "expires_at": "2016-07-11T22:14:10Z"
            }"#;
        let token = serde_json::from_str::<RawInstallationToken>(resp).unwrap();
        assert_eq!(
            token,
            RawInstallationToken {
                token: "v1.1f699f1069f60xxx".into(),
                expires_at: Utc.ymd(2016, 7, 11).and_hms(22, 14, 10),
            }
        );
    }
}
