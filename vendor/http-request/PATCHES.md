# Local patch of http-request 22.2.0

The published crate is retained with its MIT license. `[patch.crates-io]` selects
this copy; it is not a replacement client or a reqwest fallback.

Changes needed by the Web fetch bindings:

- Provide a nonzero default read buffer and timeout.
- Select TLS and the default port from the URL scheme, independently of the HTTP version.
- Expose an API to add trusted platform roots without disabling validation.
- Explicitly select rustls's ring provider, since the workspace also enables aws-lc.
- Send the configured method instead of only accepting/hardcoding GET and POST.
- Preserve request body bytes, including binary and multipart bodies, without re-encoding by MIME type.
- Handle HEAD and null-body status codes, and read close-delimited bodies to EOF.
- Parse binary response bodies without splitting/rejoining them as lines.

Redirect policy and cross-origin credential stripping live in the binding layer.
Loopback HTTP/HTTPS integration tests in sources/repository/src/net exercise
these changes. Remove this patch after upstream fixes cover these cases.
