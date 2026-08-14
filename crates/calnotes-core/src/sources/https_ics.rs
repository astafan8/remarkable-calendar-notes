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
            // Include the final (post-redirect) URL and status so a mistyped
            // path or an unexpected redirect is visible in the TEST result.
            ureq::Error::Status(code, response) => SourceError::Http(format!(
                "HTTP {code} from {}",
                response.get_url()
            )),
            other => SourceError::Http(other.to_string()),
        })?;
    response
        .into_string()
        .map_err(|e| SourceError::Parse(e.to_string()))
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
