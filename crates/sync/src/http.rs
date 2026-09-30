//! The only network code. One agent and two shapes of request: a POST of
//! JSON to a service, with the bearer token on the calls that ask for it,
//! and a GET of a picture, which never carries the token.
//! Nothing here logs a header or a body; a line names the method, the URL,
//! the status and how long the call took. Every status comes back as a
//! response, so the service module reads the reasons a 4xx carries; only a
//! failure to reach the service at all is an error.

use std::time::{Duration, Instant};

use serde_json::Value;
use tracing::{debug, warn};
use ureq::Agent;
use ureq::http::HeaderValue;

use crate::Token;
use crate::service::Failure;

const TIMEOUT: Duration = Duration::from_secs(15);

/// The most a picture may weigh. A cover is a few hundred kilobytes; a
/// body past this is not one.
const PICTURE_LIMIT: u64 = 8 * 1024 * 1024;
const USER_AGENT: &str = concat!("Ryuuji/", env!("CARGO_PKG_VERSION"));

/// A request as a service module builds it: where, what, and whether the
/// token goes with it. It never holds the token, so nothing that prints it
/// can show one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Request {
    pub url: &'static str,
    pub body: Value,
    /// Whether the `Authorization` header goes on it.
    pub signed: bool,
}

/// What came back: the status, the two headers the engine reads, and the
/// body as JSON when it was JSON. `received` is when it arrived, so a
/// `Retry-After` can be turned into a time.
#[derive(Clone, Debug)]
pub(crate) struct Response {
    pub status: u16,
    pub retry_after: Option<Duration>,
    pub rate_remaining: Option<u32>,
    pub body: Option<Value>,
    pub received: Instant,
}

impl Response {
    /// A response from a fixture: `{"status", "headers", "body"}`, where a
    /// string body is the raw text of one that was not JSON.
    #[cfg(test)]
    pub(crate) fn fixture(text: &str) -> Response {
        let fixture: Value = serde_json::from_str(text).expect("a fixture is JSON");
        let headers = fixture.get("headers").and_then(Value::as_object);
        let header = |name: &str| {
            headers?
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .and_then(|(_, value)| value.as_str())
                .map(str::to_owned)
        };
        let (retry_after, rate_remaining) = limits(header);
        let body = match fixture.get("body") {
            Some(Value::String(raw)) => serde_json::from_str(raw).ok(),
            Some(json) => Some(json.clone()),
            None => None,
        };
        Response {
            status: u16::try_from(fixture["status"].as_u64().expect("a status")).unwrap(),
            retry_after,
            rate_remaining,
            body,
            received: Instant::now(),
        }
    }
}

/// `Retry-After` as seconds and `X-RateLimit-Remaining`, from whatever
/// reads a header by name.
fn limits(header: impl Fn(&str) -> Option<String>) -> (Option<Duration>, Option<u32>) {
    let retry_after = header("retry-after")
        .and_then(|value| value.trim().parse().ok())
        .map(Duration::from_secs);
    let remaining = header("x-ratelimit-remaining").and_then(|value| value.trim().parse().ok());
    (retry_after, remaining)
}

pub(crate) struct Http {
    agent: Agent,
}

impl Http {
    pub(crate) fn new() -> Http {
        let config = Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(TIMEOUT))
            .user_agent(USER_AGENT)
            .accept("application/json")
            .build();
        Http {
            agent: config.new_agent(),
        }
    }

    /// Sends `request`, with `token` where it is signed. A failure to reach
    /// the service (connect, DNS, TLS, timeout) is `Offline`; any status is
    /// a response, with no body when the body could not be read.
    pub(crate) fn send(
        &self,
        request: &Request,
        token: Option<&Token>,
    ) -> Result<Response, Failure> {
        let started = Instant::now();
        let mut builder = self
            .agent
            .post(request.url)
            .header("Content-Type", "application/json");
        if let Some(token) = token {
            let mut value =
                HeaderValue::from_str(&format!("Bearer {}", token.secret())).map_err(|_| {
                    Failure::Refused("the token has characters a header cannot carry".into())
                })?;
            value.set_sensitive(true);
            builder = builder.header("Authorization", value);
        }
        let response = match builder.send(request.body.to_string()) {
            Ok(response) => response,
            Err(err) => {
                warn!(
                    method = "POST",
                    url = request.url,
                    elapsed_ms = elapsed_ms(started),
                    error = %err,
                    "the service could not be reached"
                );
                return Err(Failure::Offline);
            }
        };
        let status = response.status().as_u16();
        let headers = response.headers();
        let (retry_after, rate_remaining) = limits(|name| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        });
        let body = match response.into_body().read_to_string() {
            Ok(text) => serde_json::from_str(&text).ok(),
            Err(err) => {
                warn!(url = request.url, error = %err, "the answer's body could not be read");
                None
            }
        };
        debug!(
            method = "POST",
            url = request.url,
            status,
            elapsed_ms = elapsed_ms(started),
            "request"
        );
        Ok(Response {
            status,
            retry_after,
            rate_remaining,
            body,
            received: Instant::now(),
        })
    }
}

impl Http {
    /// Fetches a picture from `url`. It takes no token, so none can go with
    /// it: a picture lives on a public address, often another host's. Any
    /// status but 200 is `Transient`, and so is a body past the limit.
    pub(crate) fn fetch(&self, url: &str) -> Result<Vec<u8>, Failure> {
        let started = Instant::now();
        let response = match self.agent.get(url).header("Accept", "image/*").call() {
            Ok(response) => response,
            Err(err) => {
                warn!(method = "GET", url, elapsed_ms = elapsed_ms(started), error = %err, "the picture could not be reached");
                return Err(Failure::Offline);
            }
        };
        let status = response.status().as_u16();
        debug!(
            method = "GET",
            url,
            status,
            elapsed_ms = elapsed_ms(started),
            "picture"
        );
        if status != 200 {
            return Err(Failure::Transient);
        }
        response
            .into_body()
            .with_config()
            .limit(PICTURE_LIMIT)
            .read_to_vec()
            .map_err(|err| {
                warn!(url, error = %err, "the picture's body could not be read");
                Failure::Transient
            })
    }
}

fn elapsed_ms(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_agent_names_ryuuji_asks_for_json_and_takes_every_status_as_an_answer() {
        let config = Http::new().agent.config().clone();
        assert!(!config.http_status_as_error());
        assert_eq!(config.timeouts().global, Some(TIMEOUT));
        assert!(USER_AGENT.starts_with("Ryuuji/0."));
    }

    #[test]
    fn a_fixture_reads_its_status_headers_and_body() {
        let response = Response::fixture(
            r#"{"status": 429, "headers": {"Retry-After": "30", "x-ratelimit-remaining": " 0 "}, "body": {"data": null}}"#,
        );
        assert_eq!(response.status, 429);
        assert_eq!(response.retry_after, Some(Duration::from_secs(30)));
        assert_eq!(response.rate_remaining, Some(0));
        assert_eq!(response.body, Some(serde_json::json!({"data": null})));

        let html = Response::fixture(r#"{"status": 500, "body": "<html>500</html>"}"#);
        assert_eq!(html.status, 500);
        assert_eq!(html.body, None);
        assert_eq!((html.retry_after, html.rate_remaining), (None, None));
    }
}
