//! HTTP client with per-host concurrency limits.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use miette::Diagnostic;
use reqwest::{Response, Url, header};
use thiserror::Error;

use crate::auth::Credentials;
use tokio::{
    io::{AsyncWrite, AsyncWriteExt},
    sync::{Semaphore, SemaphorePermit},
};

/// Concurrent requests allowed per host.
const PER_HOST: usize = 4;
const GITHUB_API: &str = "api.github.com";

#[derive(Debug, Error, Diagnostic)]
pub enum HttpError {
    #[error("cannot create HTTP client")]
    #[diagnostic(code(fetchrix::http::client))]
    Client(#[source] reqwest::Error),
    #[error("invalid URL `{url}`: {message}")]
    #[diagnostic(code(fetchrix::http::url))]
    Url { url: String, message: String },
    #[error("request to {url} failed")]
    #[diagnostic(code(fetchrix::http::request))]
    Request {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("cannot save {url}")]
    #[diagnostic(code(fetchrix::http::io))]
    Io {
        url: String,
        #[source]
        source: std::io::Error,
    },
    #[error("GitHub API rate limit exceeded")]
    #[diagnostic(
        code(fetchrix::http::rate_limit),
        help("set GITHUB_TOKEN, pass --access-token github.com=TOKEN, or add github.com to netrc")
    )]
    GithubRateLimit,
}

type Result<T, E = HttpError> = std::result::Result<T, E>;

pub struct Client {
    http: reqwest::Client,
    hosts: Mutex<HashMap<String, Arc<Semaphore>>>,
    credentials: Credentials,
}

impl Client {
    /// Creates a client that authenticates requests with `credentials`.
    ///
    /// # Errors
    ///
    /// Returns an error if the TLS backend cannot be initialized.
    pub fn new(credentials: Credentials) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("fetchrix/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(Duration::from_secs(60))
            .build()
            .map_err(HttpError::Client)?;
        Ok(Self {
            http,
            hosts: Mutex::default(),
            credentials,
        })
    }

    /// Fetches `url` into memory.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails.
    pub async fn get(&self, url: &str) -> Result<Vec<u8>> {
        let mut body = Vec::new();
        self.download(url, &mut body).await?;
        Ok(body)
    }

    /// Streams the body of `url` into `out`.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or `out` cannot be written.
    ///
    /// # Panics
    ///
    /// Never; host semaphores are never closed.
    pub async fn download(&self, url: &str, out: &mut (impl AsyncWrite + Unpin)) -> Result<()> {
        let parsed = Url::parse(url).map_err(|e| HttpError::Url {
            url: url.to_owned(),
            message: e.to_string(),
        })?;
        let semaphore = self.semaphore(parsed.host_str().unwrap_or_default());
        let _permit: SemaphorePermit = semaphore
            .acquire()
            .await
            .expect("host semaphores are never closed");
        let mut response = self
            .send(parsed)
            .await
            .map_err(|source| HttpError::Request {
                url: url.to_owned(),
                source,
            })?;
        let request = |source| HttpError::Request {
            url: url.to_owned(),
            source,
        };
        let io = |source| HttpError::Io {
            url: url.to_owned(),
            source,
        };
        if response.url().host_str() == Some(GITHUB_API)
            && self.credentials.get(GITHUB_API).is_none()
            && matches!(response.status().as_u16(), 403 | 429)
        {
            return Err(HttpError::GithubRateLimit);
        }
        response = response.error_for_status().map_err(request)?;
        while let Some(chunk) = response.chunk().await.map_err(request)? {
            out.write_all(&chunk).await.map_err(io)?;
        }
        out.flush().await.map_err(io)
    }

    fn semaphore(&self, host: &str) -> Arc<Semaphore> {
        self.hosts
            .lock()
            .expect("no code panics while holding the hosts lock")
            .entry(host.to_owned())
            .or_insert_with(|| Arc::new(Semaphore::new(PER_HOST)))
            .clone()
    }

    async fn send(&self, url: Url) -> reqwest::Result<Response> {
        let host = url.host_str().unwrap_or_default().to_owned();
        let mut request = self.http.get(url);
        if host == GITHUB_API {
            request = request.header(header::ACCEPT, "application/vnd.github+json");
        }
        if let Some(credential) = self.credentials.get(&host) {
            request = credential.apply(request, &host);
        }
        request.send().await
    }
}
