# Security Policy

## Supported versions

Only the latest tagged release is supported. Please update before
reporting an issue if a newer release is available.

## Reporting a vulnerability

Please report security issues privately via GitHub's
["Report a vulnerability"](https://github.com/astafan8/remarkable-calendar-notes/security/advisories/new)
flow (Security tab → Advisories → New draft advisory) rather than a public
issue. Include:

- The affected version/commit.
- Steps to reproduce, and the impact you believe it has.
- Whether it requires physical device access, network access, or neither.

We aim to acknowledge reports within 5 business days.

## Known, accepted limitations (not vulnerabilities to report)

These are documented, intentional trade-offs — see `docs/SECURITY.md` for
full detail. Please don't file these as new reports unless you've found a
way to make the impact meaningfully worse than described:

- **Plaintext-at-rest credentials.** OAuth refresh tokens, Google client
  secrets, and iCloud app-specific passwords are stored in plaintext JSON
  under `~/.local/share/remarkable-calendar-notes/config.json`. reMarkable
  OS has no supported per-app secret store; this is a platform
  limitation, mitigated only by masking secrets in the UI and never
  logging them.
- **No sandboxing beyond AppLoad's own process model.** This app runs
  with whatever privileges AppLoad grants an external QTFB app.
- **iCloud/Google network calls use standard, unauthenticated-by-default
  TLS certificate validation** (via `rustls` + bundled `webpki-roots`);
  there is no certificate pinning.

## Scope

In scope: the Rust code in `crates/`, the packaging scripts in
`scripts/` and `.github/workflows/`, and the Vellum packaging under
`vellum/`.

Out of scope: reMarkable OS itself, AppLoad, Vellum, XOVI, and other
third-party components this project depends on but does not control —
please report issues in those upstream.
