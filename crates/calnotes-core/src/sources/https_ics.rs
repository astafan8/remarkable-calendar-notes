//! Arbitrary HTTPS `.ics` URL source, fetched with `ureq` over rustls
//! (pure-Rust TLS, bundled webpki roots via ureq's `tls` feature — no
//! OpenSSL/system certificate store dependency).

use super::SourceError;
use std::time::Duration;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

pub fn fetch_ics(url: &str) -> Result<String, SourceError> {
    if !url.starts_with("https://") {
        return Err(SourceError::Http(format!(
            "refusing non-HTTPS calendar URL: {url}"
        )));
    }
    let response = ureq::get(url)
        .timeout(REQUEST_TIMEOUT)
        .set("User-Agent", "remarkable-calendar-notes/0.1")
        .call()
        .map_err(|e| SourceError::Http(e.to_string()))?;
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
    }
}
