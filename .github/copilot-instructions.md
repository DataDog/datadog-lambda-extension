# Copilot Code Review Instructions

## Security — PII and Secrets

Flag any logging statements (`log::info!`, `log::debug!`, `log::warn!`, `log::error!`,
`tracing::info!`, `tracing::debug!`, `tracing::warn!`, `tracing::error!`, or unqualified
`info!`, `debug!`, `warn!`, `error!` macros (e.g., via `use tracing::{info, debug, warn, error}`))
that may log:
- HTTP request/response headers (Authorization, Cookie, X-API-Key, or similar)
- HTTP request/response bodies or raw payloads
- Any PII fields (e.g., email, name, user_id, ip_address, phone, ssn, date_of_birth)
- API keys, tokens, secrets, or credentials
- Structs or types that contain any of the above fields

Suggest redacting or omitting the sensitive field rather than logging it.

## Security — Unsafe Rust

Flag new `unsafe` blocks and explain what invariant the author must uphold to make the
block safe. If there is a safe alternative, suggest it.

## Security — Error Handling

Flag cases where errors are silently swallowed (empty `catch`, `.ok()` without
handling, `let _ = result`) or where operations like `.unwrap()`/`.expect()` may panic,
in code paths that handle external input or network responses.
