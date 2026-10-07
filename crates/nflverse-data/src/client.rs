use serde::Deserialize;

use crate::error::NflDataError;

const USER_AGENT: &str = concat!("sunday-slate-nfl-data/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Deserialize)]
pub(crate) struct Release {
    pub assets: Vec<ReleaseAsset>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ReleaseAsset {
    pub name: String,
    /// Opaque cache key — compared, never parsed.
    pub updated_at: String,
    pub browser_download_url: String,
}

pub(crate) struct GithubClient {
    http: reqwest::Client,
    api_base: String,
    token: Option<String>,
}

impl GithubClient {
    pub(crate) fn new(api_base: String, token: Option<String>) -> Result<Self, NflDataError> {
        let http = reqwest::Client::builder().user_agent(USER_AGENT).build()?;
        Ok(Self {
            http,
            api_base,
            token,
        })
    }

    pub(crate) async fn release(&self, tag: &str) -> Result<Release, NflDataError> {
        let url = format!(
            "{}/repos/nflverse/nflverse-data/releases/tags/{tag}",
            self.api_base
        );
        let mut req = self.http.get(&url);
        if let Some(token) = &self.token {
            req = req.bearer_auth(token);
        }
        let resp = req.send().await?;
        if !resp.status().is_success() {
            return Err(NflDataError::GitHubApi {
                status: resp.status().as_u16(),
                url,
            });
        }
        Ok(resp.json().await?)
    }

    pub(crate) async fn download(&self, url: &str) -> Result<Vec<u8>, NflDataError> {
        let resp = self.http.get(url).send().await?;
        if !resp.status().is_success() {
            return Err(NflDataError::GitHubApi {
                status: resp.status().as_u16(),
                url: url.to_string(),
            });
        }
        Ok(resp.bytes().await?.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{header, header_exists, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    #[tokio::test]
    async fn release_fetches_and_parses_assets() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/repos/nflverse/nflverse-data/releases/tags/schedules",
            ))
            .and(header_exists("user-agent"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "assets": [{
                    "name": "games.csv",
                    "updated_at": "2026-06-30T10:36:16Z",
                    "browser_download_url": "https://example.invalid/games.csv"
                }]
            })))
            .mount(&server)
            .await;

        let client = GithubClient::new(server.uri(), None).unwrap();
        let release = client.release("schedules").await.unwrap();

        assert_eq!(release.assets.len(), 1);
        assert_eq!(release.assets[0].name, "games.csv");
        assert_eq!(release.assets[0].updated_at, "2026-06-30T10:36:16Z");
    }

    #[tokio::test]
    async fn release_sends_bearer_token_when_configured() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/nflverse/nflverse-data/releases/tags/players"))
            .and(header("authorization", "Bearer sekrit"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "assets": [] })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = GithubClient::new(server.uri(), Some("sekrit".into())).unwrap();
        client.release("players").await.unwrap();
    }

    #[tokio::test]
    async fn non_success_status_is_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/nflverse/nflverse-data/releases/tags/rosters"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let client = GithubClient::new(server.uri(), None).unwrap();
        let err = client.release("rosters").await.unwrap_err();
        assert!(matches!(err, NflDataError::GitHubApi { status: 403, .. }));
    }

    #[tokio::test]
    async fn download_returns_bytes() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/download/games.csv"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"a,b\n1,2\n".to_vec()))
            .mount(&server)
            .await;

        let client = GithubClient::new(server.uri(), None).unwrap();
        let bytes = client
            .download(&format!("{}/download/games.csv", server.uri()))
            .await
            .unwrap();
        assert_eq!(bytes, b"a,b\n1,2\n");
    }
}
