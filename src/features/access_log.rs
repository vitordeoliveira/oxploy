//! The requests the apps answered with a server error, which Caddy sends oxploy as they
//! happen, one JSON line each, and which oxploy writes in each app's log. An app may
//! answer 500 without printing why, as a Topcoat app does, and Caddy answers 502 when
//! the app does not answer at all, so the app's own output would show nothing. For a
//! request Caddy could not pass on, it also sends what went wrong.

use serde::Deserialize;
use tokio::net::UdpSocket;

use super::app_files::AppFiles;
use super::caddy::ACCESS_LOGGER;
use super::runtime::note;

/// A request as Caddy logs it.
#[derive(Deserialize)]
struct Entry {
    /// The logger it went through, which names the app, such as
    /// `http.log.access.oxploy.shop-api`.
    logger: String,
    /// What went wrong, in Caddy's log of errors.
    #[serde(default)]
    msg: String,
    request: Request,
    #[serde(default)]
    status: u16,
    /// How long the answer took, in seconds.
    #[serde(default)]
    duration: f64,
}

#[derive(Deserialize)]
struct Request {
    method: String,
    uri: String,
}

/// Takes Caddy's log at `address`, on a task of its own. Caddy sends it over UDP, a
/// datagram for each line or few.
pub fn listen(address: String) {
    tokio::spawn(async move {
        let socket = match UdpSocket::bind(&address).await {
            Ok(socket) => socket,
            Err(error) => {
                eprintln!("oxploy: the apps' server errors cannot be taken from Caddy at {address}: {error}");
                return;
            }
        };
        let mut buffer = vec![0; 64 * 1024];
        loop {
            let Ok(read) = socket.recv(&mut buffer).await else {
                continue;
            };
            for line in String::from_utf8_lossy(&buffer[..read]).lines() {
                if let Some((app, text)) = describe(line) {
                    let files = AppFiles::of(&app);
                    // Only an app oxploy runs has its folder.
                    if files.root().is_dir() {
                        note(&files, &text);
                    }
                }
            }
        }
    });
}

/// The app a line of Caddy's log is about, and what to write in its log, such as
/// `GET /orders answered 500 Internal Server Error in 12 ms`. The query is left out, as
/// it may hold secrets, such as a token.
fn describe(line: &str) -> Option<(String, String)> {
    let entry: Entry = serde_json::from_str(line).ok()?;
    let error = entry
        .logger
        .strip_prefix(&format!("http.log.error.{ACCESS_LOGGER}."));
    let app = match error {
        Some(app) => app,
        None => entry
            .logger
            .strip_prefix(&format!("http.log.access.{ACCESS_LOGGER}."))?,
    };
    // An app name is a slug, which is also a safe folder name.
    let slug = !app.is_empty()
        && app
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if !slug {
        return None;
    }
    let path = match entry.request.uri.split_once('?') {
        Some((path, _)) => format!("{path}?…"),
        None => entry.request.uri,
    };
    let method = entry.request.method;
    if error.is_some() {
        return Some((app.to_owned(), format!("{method} {path} failed in Caddy: {}", entry.msg)));
    }
    let reason = ureq::http::StatusCode::from_u16(entry.status)
        .ok()
        .and_then(|status| status.canonical_reason())
        .map(|reason| format!(" {reason}"))
        .unwrap_or_default();
    let milliseconds = (entry.duration * 1000.0).round();
    Some((
        app.to_owned(),
        format!("{method} {path} answered {}{reason} in {milliseconds} ms", entry.status),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_errors_name_their_app() {
        let line = r#"{"level":"error","ts":1793820601.5,"logger":"http.log.access.oxploy.personal-blog-webpage","msg":"handled request","request":{"remote_ip":"203.0.113.9","proto":"HTTP/2.0","method":"GET","host":"blog.example.com","uri":"/posts/hello?token=secret","headers":{}},"bytes_read":0,"duration":0.0124,"size":21,"status":500,"resp_headers":{}}"#;
        assert_eq!(
            describe(line),
            Some((
                "personal-blog-webpage".to_owned(),
                "GET /posts/hello?… answered 500 Internal Server Error in 12 ms".to_owned()
            ))
        );
    }

    #[test]
    fn caddy_says_what_went_wrong() {
        let line = r#"{"level":"error","logger":"http.log.error.oxploy.visto-test","msg":"dial tcp 127.0.0.1:15042: connect: connection refused","request":{"method":"GET","uri":"/orders"},"duration":0.0004,"status":502,"err_id":"x"}"#;
        assert_eq!(
            describe(line),
            Some((
                "visto-test".to_owned(),
                "GET /orders failed in Caddy: dial tcp 127.0.0.1:15042: connect: connection refused".to_owned()
            ))
        );
    }

    #[test]
    fn other_lines_are_left_alone() {
        let panel = r#"{"logger":"http.log.access","request":{"method":"GET","uri":"/"},"duration":0.1,"status":502}"#;
        assert_eq!(describe(panel), None);
        let escaping = r#"{"logger":"http.log.access.oxploy.../etc","request":{"method":"GET","uri":"/"},"duration":0.1,"status":500}"#;
        assert_eq!(describe(escaping), None);
        assert_eq!(describe("not json"), None);
    }
}
