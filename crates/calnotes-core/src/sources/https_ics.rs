//! Arbitrary HTTPS `.ics` URL source, fetched with `ureq` over rustls
//! (pure-Rust TLS, bundled webpki roots via ureq's `tls` feature — no
//! OpenSSL/system certificate store dependency).

use super::SourceError;
use std::time::Duration;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

pub fn fetch_ics(url: &str) -> Result<String, SourceError> {
    // Tolerate stray whitespace and a mixed-case scheme so a URL the user
    // clearly meant as HTTPS is not rejected on a technicality.
    let url = url.trim();
    if !url.to_ascii_lowercase().starts_with("https://") {
        return Err(SourceError::Http(format!(
            "refusing non-HTTPS calendar URL: {url}"
        )));
    }
    let response = ureq::get(url)
        .timeout(REQUEST_TIMEOUT)
        // Some calendar hosts/CDNs return 404/403 to requests that lack a
        // browser-like User-Agent or an Accept header (even though the same
        // URL works in curl), so send both.
        .set(
            "User-Agent",
            "Mozilla/5.0 (compatible; remarkable-calendar-notes/0.1; +https://github.com/astafan8/remarkable-calendar-notes)",
        )
        .set("Accept", "text/calendar, text/plain, */*")
        .call()
        .map_err(|e| match e {
            // Surface the final (post-redirect) URL, the server, the
            // content-type, and a short body snippet — a 404's body is
            // often an explanatory HTML page (CDN block, "moved", etc.).
            ureq::Error::Status(code, response) => {
                let final_url = response.get_url().to_string();
                let server = response.header("server").unwrap_or("?").to_string();
                let ctype = response.header("content-type").unwrap_or("?").to_string();
                let snippet: String = response
                    .into_string()
                    .unwrap_or_default()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .take(180)
                    .collect();
                SourceError::Http(format!(
                    "HTTP {code} from {final_url} (server: {server}; type: {ctype}); body: {snippet}"
                ))
            }
            other => SourceError::Http(other.to_string()),
        })?;
    response
        .into_string()
        .map_err(|e| SourceError::Parse(e.to_string()))
}

/// A verbose, human-readable diagnostic of fetching `url`, used by the
/// binary's `fetch-debug` subcommand to root-cause "works in curl, fails in
/// the app" reports directly on the device. Runs the exact same ureq/rustls
/// path as [`fetch_ics`], with three header variants, and prints the status,
/// final URL, key response headers, and a body snippet for each.
pub fn fetch_ics_report(url: &str) -> String {
    let url = url.trim();
    let mut out = String::new();
    out.push_str("=== ICS fetch debug ===\n");
    out.push_str(&format!("requested URL: {url}\n"));
    if !url.to_ascii_lowercase().starts_with("https://") {
        out.push_str("NOTE: not an https:// URL — the app rejects these before fetching.\n");
    }
    out.push_str("client: ureq 2.x over rustls (bundled webpki roots)\n\n");

    let probes: [(&str, Option<&str>, Option<&str>); 3] = [
        (
            "app headers",
            Some("Mozilla/5.0 (compatible; remarkable-calendar-notes/0.1; +https://github.com/astafan8/remarkable-calendar-notes)"),
            Some("text/calendar, text/plain, */*"),
        ),
        ("curl-like UA, no Accept", Some("curl/8.5.0"), None),
        ("no custom headers", None, None),
    ];
    for (label, ua, accept) in probes {
        out.push_str(&format!("--- probe: {label} ---\n"));
        let mut req = ureq::get(url).timeout(REQUEST_TIMEOUT);
        if let Some(ua) = ua {
            req = req.set("User-Agent", ua);
        }
        if let Some(accept) = accept {
            req = req.set("Accept", accept);
        }
        match req.call() {
            Ok(response) => out.push_str(&describe_response(response)),
            Err(ureq::Error::Status(code, response)) => {
                out.push_str(&format!("status: {code} (HTTP error)\n"));
                out.push_str(&describe_response(response));
            }
            Err(ureq::Error::Transport(t)) => {
                out.push_str(&format!("TRANSPORT ERROR (no HTTP response): {t}\n"));
            }
        }
        out.push('\n');
    }
    out
}

fn describe_response(response: ureq::Response) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "status: {} {}\n",
        response.status(),
        response.status_text()
    ));
    out.push_str(&format!("final URL: {}\n", response.get_url()));
    for name in [
        "server",
        "content-type",
        "content-length",
        "location",
        "cf-ray",
        "cf-mitigated",
        "via",
        "x-cache",
        "set-cookie",
    ] {
        if let Some(value) = response.header(name) {
            out.push_str(&format!("  {name}: {value}\n"));
        }
    }
    match response.into_string() {
        Ok(body) => {
            let bytes = body.len();
            let looks_like_ics = body.contains("BEGIN:VCALENDAR");
            let snippet: String = body.chars().take(400).collect();
            out.push_str(&format!(
                "body bytes: {bytes} | contains BEGIN:VCALENDAR: {looks_like_ics}\n"
            ));
            out.push_str(&format!("body (first 400 chars):\n{snippet}\n"));
        }
        Err(e) => out.push_str(&format!("body read error: {e}\n")),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_https_urls_before_making_any_request() {
        let err = fetch_ics("http://example.com/cal.ics").unwrap_err();
        assert!(matches!(err, SourceError::Http(_)));
        // Whitespace and case do not sneak an http URL past the guard.
        assert!(fetch_ics("  HTTP://example.com/cal.ics ").is_err());
    }
}
