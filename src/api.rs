use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

use crate::config::normalize_server;
use crate::models::{
    Bookmark, BookmarkPatch, BookmarkQuery, Collection, Label, Page, Profile,
    SyncChange,
};

const OAUTH_CLIENT_URI: &str = "https://github.com/gollyhatch/eckly";
const OAUTH_SOFTWARE_ID: &str = "com.github.gollyhatch.eckly";

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("{message}")]
    Status { status: u16, message: String },

    #[error("Network error: {0}")]
    Request(#[from] reqwest::Error),

    #[error("Invalid server URL: {0}")]
    InvalidUrl(String),

    #[error("Could not parse the server response: {0}")]
    Decode(#[from] serde_json::Error),

    #[error("This bookmark has no readable article.")]
    NoArticle,

    #[error("Image error: {0}")]
    Image(String),

    #[error("OAuth error: {message}")]
    OAuth { code: String, message: String },
}

#[derive(Debug, Clone, Deserialize)]
pub struct OAuthDevice {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    #[serde(default)]
    pub expires_in: u64,
    #[serde(default = "default_poll_interval")]
    pub interval: u64,
}

fn default_poll_interval() -> u64 {
    5
}

/// Result of polling the OAuth token endpoint during the device flow.
#[derive(Debug, Clone)]
pub enum OAuthPoll {
    Pending,
    SlowDown,
    Token(String),
    Denied(String),
    Expired(String),
}

impl ApiError {
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Status { status, .. } => Some(*status),
            Self::NoArticle => Some(404),
            _ => None,
        }
    }

    pub fn is_no_article(&self) -> bool {
        matches!(self, Self::NoArticle)
    }

    pub fn is_network(&self) -> bool {
        matches!(self, Self::Request(_))
    }
}

/// Async client for the Readeck HTTP API.
#[derive(Clone)]
pub struct ReadeckClient {
    http: reqwest::Client,
    base: Arc<str>,
    token: Arc<str>,
}

impl ReadeckClient {
    fn build(server: &str, token: &str) -> Result<Self, ApiError> {
        let base = normalize_server(server)
            .map_err(|err| ApiError::InvalidUrl(err.to_string()))?;
        // Readeck adds one `Link: ...; rel="media"` header per imported
        // resource to article responses. Pages with many images exceed
        // hyper's limits (100 headers / 16 KB), so allow generous ones.
        let http = reqwest::Client::builder()
            .user_agent(concat!("eckly/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .http1_max_headers(1024)
            .http2_max_header_list_size(1024 * 1024)
            .build()?;

        Ok(Self {
            http,
            base: Arc::from(base),
            token: Arc::from(token.trim()),
        })
    }

    pub fn new(server: &str, token: &str) -> Result<Self, ApiError> {
        if token.trim().is_empty() {
            return Err(ApiError::InvalidUrl("API token is empty".into()));
        }
        Self::build(server, token)
    }

    /// Client used for the public OAuth device flow, before a token exists.
    pub fn new_oauth(server: &str) -> Result<Self, ApiError> {
        Self::build(server, "")
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}/api{}", self.base, path)
    }

    async fn send_ok(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, ApiError> {
        let response = request.send().await?;
        ensure_ok(response).await
    }

    /// Register an ephemeral OAuth client for the device code flow.
    pub async fn oauth_register(&self) -> Result<String, ApiError> {
        let response = self
            .http
            .post(self.endpoint("/oauth/client"))
            .json(&serde_json::json!({
                "client_name": "eckly",
                "client_uri": OAUTH_CLIENT_URI,
                "software_id": OAUTH_SOFTWARE_ID,
                "software_version": env!("CARGO_PKG_VERSION"),
                "grant_types": ["urn:ietf:params:oauth:grant-type:device_code"],
                "token_endpoint_auth_method": "none",
            }))
            .send()
            .await?;
        let (status, value) = oauth_json(response).await?;
        if !(200..300).contains(&status) {
            return Err(oauth_error(&value));
        }
        value
            .get("client_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| ApiError::OAuth {
                code: "invalid_response".into(),
                message: "OAuth registration returned no client_id".into(),
            })
    }

    /// Request a device code and user verification code.
    pub async fn oauth_device(
        &self,
        client_id: &str,
    ) -> Result<OAuthDevice, ApiError> {
        let response = self
            .http
            .post(self.endpoint("/oauth/device"))
            .json(&serde_json::json!({
                "client_id": client_id,
                "scope": "bookmarks:read bookmarks:write profile:read",
            }))
            .send()
            .await?;
        let (status, value) = oauth_json(response).await?;
        if !(200..300).contains(&status) {
            return Err(oauth_error(&value));
        }
        Ok(serde_json::from_value(value)?)
    }

    /// Poll the token endpoint once during the device flow.
    pub async fn oauth_token(
        &self,
        client_id: &str,
        device_code: &str,
    ) -> Result<OAuthPoll, ApiError> {
        let response = self
            .http
            .post(self.endpoint("/oauth/token"))
            .json(&serde_json::json!({
                "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
                "device_code": device_code,
                "client_id": client_id,
            }))
            .send()
            .await?;
        let (status, value) = oauth_json(response).await?;

        if (200..300).contains(&status) {
            return value
                .get("access_token")
                .and_then(Value::as_str)
                .map(|token| OAuthPoll::Token(token.to_string()))
                .ok_or_else(|| ApiError::OAuth {
                    code: "invalid_response".into(),
                    message: "OAuth token response returned no access_token"
                        .into(),
                });
        }

        let code = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("oauth_error");
        let description = value
            .get("error_description")
            .and_then(Value::as_str)
            .unwrap_or(code)
            .to_string();
        match code {
            "authorization_pending" => Ok(OAuthPoll::Pending),
            "slow_down" => Ok(OAuthPoll::SlowDown),
            "access_denied" => Ok(OAuthPoll::Denied(description)),
            "expired_token" => Ok(OAuthPoll::Expired(description)),
            _ => Err(ApiError::OAuth {
                code: code.to_string(),
                message: description,
            }),
        }
    }

    pub async fn profile(&self) -> Result<Profile, ApiError> {
        let response = self
            .send_ok(
                self.http
                    .get(self.endpoint("/profile"))
                    .bearer_auth(self.token.as_ref()),
            )
            .await?;
        Ok(response.json::<Profile>().await?)
    }

    pub async fn list_bookmarks(
        &self,
        query: &BookmarkQuery,
    ) -> Result<Page<Bookmark>, ApiError> {
        let response = self
            .http
            .get(self.endpoint("/bookmarks"))
            .bearer_auth(self.token.as_ref())
            .query(&query.params())
            .send()
            .await?;

        let total = header_usize(&response, "Total-Count").unwrap_or(0);

        let response = ensure_ok(response).await?;
        let items = response.json::<Vec<Bookmark>>().await?;

        Ok(Page {
            total: if total == 0 { items.len() } else { total },
            items,
        })
    }

    /// Metadata for a single bookmark.
    pub async fn bookmark(&self, id: &str) -> Result<Bookmark, ApiError> {
        let response = self
            .send_ok(
                self.http
                    .get(self.endpoint(&format!("/bookmarks/{id}")))
                    .bearer_auth(self.token.as_ref()),
            )
            .await?;
        Ok(response.json::<Bookmark>().await?)
    }

    /// The server's change log: bookmarks updated or deleted since a time.
    ///
    /// Without `since` the endpoint lists every bookmark as an update and
    /// omits deletions, which makes it a complete snapshot of the library.
    ///
    /// The server's clock at response time is returned with the entries so
    /// callers can use it as the next `since`.
    pub async fn sync_changes(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> Result<(Vec<SyncChange>, Option<DateTime<Utc>>), ApiError> {
        let mut request = self
            .http
            .get(self.endpoint("/bookmarks/sync"))
            .bearer_auth(self.token.as_ref());
        if let Some(since) = since {
            request = request.query(&[(
                "since",
                since.to_rfc3339_opts(SecondsFormat::Nanos, true),
            )]);
        }
        let response = ensure_ok(request.send().await?).await?;
        let time = response
            .headers()
            .get(reqwest::header::DATE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| DateTime::parse_from_rfc2822(value).ok())
            .map(|value| value.with_timezone(&Utc));
        let changes = response.json::<Vec<SyncChange>>().await?;
        Ok((changes, time))
    }

    /// The article body is an HTML fragment rather than a complete document.
    pub async fn article(&self, id: &str) -> Result<String, ApiError> {
        let response = self
            .http
            .get(self.endpoint(&format!("/bookmarks/{id}/article")))
            .bearer_auth(self.token.as_ref())
            .send()
            .await?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(ApiError::NoArticle);
        }
        let response = ensure_ok(response).await?;
        Ok(response.text().await?)
    }

    pub async fn create_bookmark(
        &self,
        url: &str,
    ) -> Result<Option<String>, ApiError> {
        let response = self
            .http
            .post(self.endpoint("/bookmarks"))
            .bearer_auth(self.token.as_ref())
            .json(&serde_json::json!({ "url": url }))
            .send()
            .await?;

        let id = response
            .headers()
            .get("Bookmark-Id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
            .or_else(|| {
                response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|location| location.rsplit('/').next())
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            });

        ensure_ok(response).await?;
        Ok(id)
    }

    pub async fn update_bookmark(
        &self,
        id: &str,
        patch: &BookmarkPatch,
    ) -> Result<(), ApiError> {
        if patch.is_empty() {
            return Ok(());
        }
        self.send_ok(
            self.http
                .patch(self.endpoint(&format!("/bookmarks/{id}")))
                .bearer_auth(self.token.as_ref())
                .json(patch),
        )
        .await?;
        Ok(())
    }

    /// Apply the same patch to several bookmarks.
    ///
    /// Requests are sent sequentially so a large selection does not flood the
    /// server. The first error is returned after attempting every bookmark;
    /// callers can refresh afterwards to see which updates succeeded.
    pub async fn update_bookmarks(
        &self,
        ids: &[String],
        patch: &BookmarkPatch,
    ) -> Result<usize, ApiError> {
        let mut updated = 0;
        let mut first_error = None;
        for id in ids {
            match self.update_bookmark(id, patch).await {
                Ok(()) => updated += 1,
                Err(err) => {
                    if first_error.is_none() {
                        first_error = Some(err);
                    }
                }
            }
        }
        match first_error {
            Some(err) => Err(err),
            None => Ok(updated),
        }
    }

    pub async fn delete_bookmark(&self, id: &str) -> Result<(), ApiError> {
        self.send_ok(
            self.http
                .delete(self.endpoint(&format!("/bookmarks/{id}")))
                .bearer_auth(self.token.as_ref()),
        )
        .await?;
        Ok(())
    }

    /// Download an image referenced by an article.
    ///
    /// Readeck rewrites article image URLs to its public media route, so no
    /// bearer token is sent for these requests.
    pub async fn fetch_media(&self, source: &str) -> Result<Vec<u8>, ApiError> {
        let base = url::Url::parse(&self.base)
            .map_err(|err| ApiError::Image(err.to_string()))?;
        let url = base
            .join(source)
            .map_err(|err| ApiError::Image(err.to_string()))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(ApiError::Image("unsupported image URL".into()));
        }
        let response = self.send_ok(self.http.get(url)).await?;
        let bytes = response.bytes().await?;
        if bytes.len() > 15 * 1024 * 1024 {
            return Err(ApiError::Image("image is larger than 15 MB".into()));
        }
        Ok(bytes.to_vec())
    }

    /// Delete several bookmarks, returning the first error after attempting
    /// every bookmark.
    pub async fn delete_bookmarks(
        &self,
        ids: &[String],
    ) -> Result<usize, ApiError> {
        let mut deleted = 0;
        let mut first_error = None;
        for id in ids {
            match self.delete_bookmark(id).await {
                Ok(()) => deleted += 1,
                Err(err) => {
                    if first_error.is_none() {
                        first_error = Some(err);
                    }
                }
            }
        }
        match first_error {
            Some(err) => Err(err),
            None => Ok(deleted),
        }
    }

    pub async fn labels(&self) -> Result<Vec<Label>, ApiError> {
        let response = self
            .send_ok(
                self.http
                    .get(self.endpoint("/bookmarks/labels"))
                    .bearer_auth(self.token.as_ref()),
            )
            .await?;
        Ok(response.json::<Vec<Label>>().await?)
    }

    pub async fn collections(&self) -> Result<Vec<Collection>, ApiError> {
        let response = self
            .send_ok(
                self.http
                    .get(self.endpoint("/bookmarks/collections"))
                    .bearer_auth(self.token.as_ref()),
            )
            .await?;
        Ok(response.json::<Vec<Collection>>().await?)
    }
}

fn header_usize(response: &reqwest::Response, name: &str) -> Option<usize> {
    response
        .headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
}

async fn oauth_json(
    response: reqwest::Response,
) -> Result<(u16, Value), ApiError> {
    let status = response.status().as_u16();
    let body = response.text().await?;
    match serde_json::from_str::<Value>(&body) {
        Ok(value) => Ok((status, value)),
        Err(err) if (200..300).contains(&status) => Err(err.into()),
        Err(_) => Ok((status, Value::String(body))),
    }
}

fn oauth_error(value: &Value) -> ApiError {
    let code = value
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("oauth_error");
    let message = value
        .get("error_description")
        .and_then(Value::as_str)
        .or_else(|| value.get("error").and_then(Value::as_str))
        .or_else(|| value.as_str())
        .unwrap_or("OAuth request failed")
        .to_string();
    ApiError::OAuth {
        code: code.to_string(),
        message,
    }
}

async fn ensure_ok(
    response: reqwest::Response,
) -> Result<reqwest::Response, ApiError> {
    if response.status().is_success() {
        return Ok(response);
    }

    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let message = error_message(status.as_u16(), &body);
    Err(ApiError::Status {
        status: status.as_u16(),
        message,
    })
}

/// Turn an error payload into a human-readable message.
///
/// Readeck returns `{"message": "..."}` for general errors and a validation
/// object with `errors` and per-field messages for 422 responses.
fn error_message(status: u16, body: &str) -> String {
    if status == 401 {
        return "Authentication failed. Check your Readeck API token.".into();
    }
    if status == 403 {
        return "This API token does not have permission for that action."
            .into();
    }

    if let Ok(value) = serde_json::from_str::<Value>(body) {
        if let Some(message) = value.get("message").and_then(Value::as_str) {
            if !message.trim().is_empty() {
                return message.trim().to_string();
            }
        }

        let mut messages = Vec::new();
        if let Some(errors) = value.get("errors").and_then(Value::as_array) {
            messages.extend(
                errors
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
            );
        }
        if let Some(fields) = value.get("fields").and_then(Value::as_object) {
            for (field, info) in fields {
                if let Some(errors) =
                    info.get("errors").and_then(Value::as_array)
                {
                    for error in errors.iter().filter_map(Value::as_str) {
                        if !error.trim().is_empty() {
                            messages.push(format!("{field}: {}", error.trim()));
                        }
                    }
                }
            }
        }
        if !messages.is_empty() {
            return messages.join("; ");
        }
        if let Some(error) = value.get("error").and_then(Value::as_str) {
            if !error.trim().is_empty() {
                return error.trim().to_string();
            }
        }
    }

    let fallback = body.trim();
    if fallback.is_empty() {
        reqwest::StatusCode::from_u16(status)
            .ok()
            .and_then(|s| s.canonical_reason())
            .unwrap_or("request failed")
            .to_string()
    } else {
        let fallback = fallback.replace('\n', " ");
        truncate(&fallback, 240)
    }
}

fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let mut out: String = value.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Filter, Sort, SyncChangeKind};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn formats_error_payloads() {
        assert_eq!(
            error_message(401, ""),
            "Authentication failed. Check your Readeck API token."
        );
        assert_eq!(
            error_message(404, r#"{"message":"not found"}"#),
            "not found"
        );
        assert_eq!(
            error_message(
                422,
                r#"{"is_valid":false,"errors":["bad input"],"fields":{"url":{"errors":["must be a URL"]}}}"#
            ),
            "bad input; url: must be a URL"
        );
        assert_eq!(error_message(500, ""), "Internal Server Error");
    }

    #[test]
    fn truncates_long_messages() {
        let msg = error_message(500, &"x".repeat(300));
        assert_eq!(msg.chars().count(), 241);
        assert!(msg.ends_with('…'));
    }

    #[test]
    fn rejects_bad_credentials() {
        assert!(ReadeckClient::new("https://example.com", "").is_err());
        assert!(ReadeckClient::new("not a url with spaces", "token").is_err());
    }

    #[tokio::test]
    async fn lists_bookmarks_over_http() {
        let body = r#"[{
            "id":"one","url":"https://example.com","title":"First",
            "read_progress":0,"labels":["rust"],"has_article":true
        }]"#;
        let (base, server) = serve_once(
            response(
                "200 OK",
                "Total-Count: 42\r\nTotal-Pages: 1\r\nCurrent-Page: 1\r\n",
                body,
            ),
            false,
        )
        .await;

        let client = ReadeckClient::new(&base, "secret-token").unwrap();
        let query =
            BookmarkQuery::new(&Filter::Unread, "rust", Sort::Newest, 50, 0);
        let page = client.list_bookmarks(&query).await.unwrap();
        assert_eq!(page.total, 42);
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].display_title(), "First");

        let request = server.await.unwrap();
        assert!(request.starts_with("GET /api/bookmarks?"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer secret-token")
        );
        assert!(!request.contains("read_status="));
        assert!(request.contains("is_archived=false"));
        assert!(request.contains("search=rust"));
        assert!(request.contains("sort=-created"));
    }

    #[tokio::test]
    async fn sends_patch_payloads() {
        let (base, server) = serve_once(
            response(
                "200 OK",
                "",
                r#"{"id":"one","updated":"2026-01-01T00:00:00Z"}"#,
            ),
            true,
        )
        .await;

        let client = ReadeckClient::new(&base, "secret-token").unwrap();
        let patch = BookmarkPatch {
            is_marked: Some(true),
            labels: Some(vec!["later".into()]),
            ..Default::default()
        };
        client.update_bookmark("one", &patch).await.unwrap();

        let request = server.await.unwrap();
        assert!(request.starts_with("PATCH /api/bookmarks/one "));
        assert!(request.contains("\"is_marked\":true"));
        assert!(request.contains("\"labels\":[\"later\"]"));
    }

    #[tokio::test]
    async fn oauth_registration_and_device_flow() {
        let (base, server) = serve_once(
            response("201 Created", "", r#"{"client_id":"client-1"}"#),
            true,
        )
        .await;
        let client = ReadeckClient::new_oauth(&base).unwrap();
        assert_eq!(client.oauth_register().await.unwrap(), "client-1");
        let request = server.await.unwrap();
        assert!(request.starts_with("POST /api/oauth/client "));
        assert!(request.contains("device_code"));
        assert!(request.contains("token_endpoint_auth_method"));

        let (base, server) = serve_once(
            response(
                "200 OK",
                "",
                r#"{"device_code":"dev","user_code":"ABCD1234","verification_uri":"https://example.com/device","expires_in":300,"interval":5}"#,
            ),
            true,
        )
        .await;
        let client = ReadeckClient::new_oauth(&base).unwrap();
        let device = client.oauth_device("client-1").await.unwrap();
        assert_eq!(device.device_code, "dev");
        assert_eq!(device.user_code, "ABCD1234");
        assert_eq!(device.interval, 5);
        let request = server.await.unwrap();
        assert!(request.starts_with("POST /api/oauth/device "));
        assert!(request.contains("bookmarks:read"));
    }

    #[tokio::test]
    async fn oauth_token_polling() {
        let (base, server) = serve_once(
            response(
                "400 Bad Request",
                "",
                r#"{"error":"authorization_pending"}"#,
            ),
            true,
        )
        .await;
        let client = ReadeckClient::new_oauth(&base).unwrap();
        assert!(matches!(
            client.oauth_token("client-1", "dev").await.unwrap(),
            OAuthPoll::Pending
        ));
        let _ = server.await;

        let (base, server) = serve_once(
            response(
                "201 Created",
                "",
                r#"{"access_token":"token-1","token_type":"Bearer"}"#,
            ),
            true,
        )
        .await;
        let client = ReadeckClient::new_oauth(&base).unwrap();
        match client.oauth_token("client-1", "dev").await.unwrap() {
            OAuthPoll::Token(token) => assert_eq!(token, "token-1"),
            other => panic!("unexpected poll result: {other:?}"),
        }
        let _ = server.await;
    }

    #[tokio::test]
    async fn fetches_media_without_authorization() {
        let (base, server) =
            serve_once(response("200 OK", "", "PNG-DATA"), false).await;
        let client = ReadeckClient::new(&base, "secret-token").unwrap();

        let bytes = client.fetch_media("/bm/aa/bb/img/pic.png").await.unwrap();
        assert_eq!(bytes, b"PNG-DATA");

        let request = server.await.unwrap();
        assert!(request.starts_with("GET /bm/aa/bb/img/pic.png "));
        assert!(
            !request
                .to_ascii_lowercase()
                .contains("authorization: bearer")
        );
    }

    #[tokio::test]
    async fn maps_missing_article() {
        let (base, server) = serve_once(
            response("404 Not Found", "", r#"{"message":"not found"}"#),
            false,
        )
        .await;

        let client = ReadeckClient::new(&base, "secret-token").unwrap();
        let error = client.article("missing").await.unwrap_err();
        assert!(error.is_no_article());
        assert_eq!(error.status(), Some(404));
        let _ = server.await;
    }

    #[tokio::test]
    async fn reads_the_change_log_and_server_time() {
        let body = r#"[
            {"id":"one","time":"2026-11-14T21:18:09.039Z","type":"delete"},
            {"id":"two","time":"2026-11-14T21:18:10.123456789Z","type":"update"}
        ]"#;
        let (base, server) = serve_once(
            response("200 OK", "Date: Sat, 14 Nov 2026 21:18:11 GMT\r\n", body),
            false,
        )
        .await;

        let client = ReadeckClient::new(&base, "secret-token").unwrap();
        let since = DateTime::parse_from_rfc3339("2026-11-14T20:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let (changes, time) = client.sync_changes(Some(since)).await.unwrap();
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].kind, SyncChangeKind::Delete);
        assert_eq!(changes[1].kind, SyncChangeKind::Update);
        assert_eq!(time.unwrap().to_rfc3339(), "2026-11-14T21:18:11+00:00");

        let request = server.await.unwrap();
        assert!(request.starts_with("GET /api/bookmarks/sync?"));
        assert!(request.contains("since=2026-11-14T20%3A00%3A00.000000000Z"));
    }

    #[tokio::test]
    async fn accepts_article_responses_with_many_link_headers() {
        let links: String = (0..150)
            .map(|index| {
                format!(
                    "Link: <https://example.com/{index}.jpg>; rel=\"media\"\r\n"
                )
            })
            .collect();
        let (base, server) =
            serve_once(response("200 OK", &links, "<p>body</p>"), false).await;

        let client = ReadeckClient::new(&base, "secret-token").unwrap();
        assert_eq!(client.article("one").await.unwrap(), "<p>body</p>");
        let _ = server.await;
    }

    async fn serve_once(
        reply: String,
        read_body: bool,
    ) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut header_end = None;
            loop {
                let mut chunk = [0u8; 1024];
                let read = socket.read(&mut chunk).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
                if header_end.is_none() {
                    header_end = find_header_end(&request);
                }
                if let Some(end) = header_end {
                    let content_length =
                        content_length(&request[..end]).unwrap_or(0);
                    if !read_body || request.len() >= end + 4 + content_length {
                        break;
                    }
                }
            }
            socket.write_all(reply.as_bytes()).await.unwrap();
            socket.shutdown().await.ok();
            String::from_utf8_lossy(&request).to_string()
        });
        (format!("http://{address}"), handle)
    }

    fn find_header_end(bytes: &[u8]) -> Option<usize> {
        bytes.windows(4).position(|window| window == b"\r\n\r\n")
    }

    fn content_length(headers: &[u8]) -> Option<usize> {
        let headers = String::from_utf8_lossy(headers);
        headers.lines().find_map(|line| {
            line.strip_prefix("Content-Length:")
                .or_else(|| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse().ok())
        })
    }

    fn response(status: &str, extra_headers: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{extra_headers}\r\n{body}",
            body.len()
        )
    }
}
