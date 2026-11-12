//! Per-host HTTP credentials.

use std::{collections::HashMap, fmt, str::FromStr};

use miette::Diagnostic;
use reqwest::RequestBuilder;
use thiserror::Error;

#[derive(Debug, Error, Diagnostic)]
pub enum AuthError {
    #[error("cannot read netrc file")]
    #[diagnostic(code(fetchrix::auth::netrc))]
    Netrc(#[source] netrc::Error),
}

#[derive(Clone)]
pub enum Credential {
    Bearer(String),
    Basic { login: String, password: String },
}

impl Credential {
    /// Git smart-HTTP endpoints reject bearer auth, so tokens for non-API
    /// hosts go as basic auth, like git and Nix do.
    pub(crate) fn apply(&self, request: RequestBuilder, host: &str) -> RequestBuilder {
        match self {
            Self::Bearer(token) if host.starts_with("api.") => request.bearer_auth(token),
            Self::Bearer(token) => request.basic_auth("x-access-token", Some(token)),
            Self::Basic { login, password } => request.basic_auth(login, Some(password)),
        }
    }
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Bearer(_) => "Bearer(..)",
            Self::Basic { .. } => "Basic(..)",
        })
    }
}

/// A `HOST=TOKEN` pair given on the command line.
#[derive(Clone)]
pub struct HostToken {
    pub host: String,
    pub token: String,
}

impl FromStr for HostToken {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.split_once('=') {
            Some((host, token)) if !host.is_empty() && !token.is_empty() => Ok(Self {
                host: host.to_ascii_lowercase(),
                token: token.to_owned(),
            }),
            _ => Err("expected HOST=TOKEN".to_owned()),
        }
    }
}

impl fmt::Debug for HostToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}=..", self.host)
    }
}

#[derive(Debug, Default)]
pub struct Credentials(HashMap<String, Credential>);

impl Credentials {
    /// Collects credentials; earlier sources win: `tokens`, then
    /// environment tokens (see [`env_tokens`]), then the netrc file.
    ///
    /// # Errors
    ///
    /// Returns an error if an existing netrc file cannot be read or parsed.
    pub fn load(tokens: &[HostToken]) -> Result<Self, AuthError> {
        let mut credentials = Self::default();
        for HostToken { host, token } in tokens {
            credentials.insert(host, Credential::Bearer(token.clone()));
        }
        for (host, token) in env_tokens(|var| std::env::var(var).ok()) {
            credentials.insert(&host, Credential::Bearer(token));
        }
        if let Some(path) = netrc::Netrc::get_file() {
            let netrc = netrc::Netrc::from_file(&path).map_err(AuthError::Netrc)?;
            credentials.merge_netrc(netrc);
        }
        Ok(credentials)
    }

    fn insert(&mut self, host: &str, credential: Credential) {
        self.0.entry(host.to_owned()).or_insert(credential);
    }

    fn merge_netrc(&mut self, netrc: netrc::Netrc) {
        for (host, auth) in netrc.hosts {
            self.insert(
                &host.to_ascii_lowercase(),
                Credential::Basic {
                    login: auth.login,
                    password: auth.password,
                },
            );
        }
    }

    /// Credential for `host`; API hosts such as `api.github.com` fall back to
    /// the bare forge host, matching Nix `access-tokens`.
    #[must_use]
    pub fn get(&self, host: &str) -> Option<&Credential> {
        self.0
            .get(host)
            .or_else(|| host.strip_prefix("api.").and_then(|bare| self.0.get(bare)))
    }
}

/// Tokens from the environment, in priority order.
///
/// `GITHUB_TOKEN` belongs to the host in `GITHUB_SERVER_URL`, which GitHub,
/// Gitea, and Forgejo Actions set; this keeps a Forgejo runner token away from
/// github.com. `GH_TOKEN` follows the `gh` CLI and always means github.com.
fn env_tokens(var: impl Fn(&str) -> Option<String>) -> Vec<(String, String)> {
    let var = |name: &str| var(name).filter(|v| !v.is_empty());
    let server = var("GITHUB_SERVER_URL")
        .and_then(|url| reqwest::Url::parse(&url).ok())
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .unwrap_or_else(|| "github.com".to_owned());
    [
        ("GITHUB_TOKEN", server),
        ("GH_TOKEN", "github.com".to_owned()),
    ]
    .into_iter()
    .filter_map(|(name, host)| var(name).map(|token| (host, token)))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&str, &str)]) -> impl Fn(&str) -> Option<String> + 'a {
        |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    #[test]
    fn github_token_follows_server_url() {
        let pairs = [
            ("GITHUB_TOKEN", "runner"),
            ("GITHUB_SERVER_URL", "https://Codeberg.org"),
            ("GH_TOKEN", "gh"),
        ];
        assert_eq!(
            env_tokens(env(&pairs)),
            [
                ("codeberg.org".to_owned(), "runner".to_owned()),
                ("github.com".to_owned(), "gh".to_owned()),
            ]
        );
        assert_eq!(
            env_tokens(env(&[("GITHUB_TOKEN", "t"), ("GH_TOKEN", "")])),
            [("github.com".to_owned(), "t".to_owned())]
        );
    }

    #[test]
    fn earlier_sources_win_and_api_hosts_fall_back() {
        let mut credentials = Credentials::default();
        let tokens: Vec<HostToken> = vec!["GitHub.com=cli".parse().unwrap()];
        for HostToken { host, token } in &tokens {
            credentials.insert(host, Credential::Bearer(token.clone()));
        }
        credentials.merge_netrc(
            "machine github.com login u password p\nmachine example.org login u password p"
                .parse()
                .unwrap(),
        );
        assert!(matches!(
            credentials.get("api.github.com"),
            Some(Credential::Bearer(t)) if t == "cli"
        ));
        assert!(matches!(
            credentials.get("example.org"),
            Some(Credential::Basic { .. })
        ));
        assert!(credentials.get("gitlab.com").is_none());
    }

    #[test]
    fn bearer_token_uses_basic_auth_for_git_hosts() {
        let credential = Credential::Bearer("t".to_owned());
        let client = reqwest::Client::builder()
            .tls_certs_only([])
            .build()
            .unwrap();
        let auth = |host: &str| {
            credential
                .apply(client.get(format!("https://{host}/")), host)
                .build()
                .unwrap()
                .headers()[reqwest::header::AUTHORIZATION]
                .to_str()
                .unwrap()
                .to_owned()
        };
        assert_eq!(auth("api.github.com"), "Bearer t");
        assert!(auth("github.com").starts_with("Basic "));
    }

    #[test]
    fn rejects_malformed_host_token() {
        assert!("github.com".parse::<HostToken>().is_err());
        assert!("=x".parse::<HostToken>().is_err());
    }
}
