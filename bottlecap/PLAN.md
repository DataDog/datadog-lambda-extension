# Enhancing test coverage

## Context & Goals

We are set to ensure sufficient unit test coverage is in place around the "Serverless App & API Protection" (also known
as "Serverless AppSec" or "Serverless AAP") functionality. The core functionality is implemented at `./src/appsec` and
is leveraged from `./src/proxy/interceptor.rs` with interactions in the `./src/lifecycle/infocation/processor.rs` as
well as `./src/lifecycle/invocation/context.rs`.

The crate under test is an AWS Lambda extension that has to major entry point components when it comes to Serverless
AAP:
1. The standard extension, which is notified about lifecycle events, function invocations, etc...
2. A Lambda Runtime API proxy, which relays calls from the customer's call to the Lambda Runtime API, so it can observe
   the request and response payloads even when Universal Service Instrumentation is not available.

This codebase is using the "inline test module" style for testing Rust codebases, where a `tests` module is present at
the end of the tested unit file that looks like so:

```rust
#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_description() {
    // Test case implementation
  }
}
```

As usual with Rust codebases, tests can be run using the `cargo test` command. More interestingly, it is possible to run
tests and producing an LCov-formatted coverage report in `./target/lcov.info` using the following command:
```console
$ cargo +nightly llvm-cov test --all-targets --branch --quiet --lcov --output-path=target/lcov.info
```

We aim to reach at least 85% line coverage and 75% branch coverage of the "Serverless AAP" code. More coverage is
better, but we don't want to waste time testing extreme edge cases, especially if they require excessive amounts of
set-up to achieve.

## Specific Instructions

- Unless instructed to in a specific case, only add tests
  - DO NOT modify existing tests
  - DO NOT modify the implementation, even if it fails a test that it should normally pass -- if that was the case,
    try to explain as clearly as possible why the test fails & yield control back to me so I can fix it before resuming.
  - DO NOT add new implementation code
- Keep the tests nice and focused; place the tests in the most relevant file (i.e, tests for body parsing should be
  added to `./src/appsec/payload/body.rs`)
- Name the test cases appropriately - if one fails, its name should help get a sense of what's broken
- Focus on the Serverless AAP feature only; we are not responsible for the rest of the surface of this crate and would
  not want to step on someone else's turf
- The Serverless AAP feature should not cause a request to fail - tests should verify this. This means we want to test
  broken and unsupported inputs; not just what is expected to be supported.
- The feature should NEVER lead to a `panic`: if a code path might cause a panic, craft a test case to demonstrate the
  `panic` condition & suggest a code change to prevent the `panic` from occurring.
- Make sure to regularly check the coverage situation (with the `cargo +nightly llvm-cov` command above) to assess where
  to extend effort.

## High-level Checklist

- [ ] `crate::appsec::Processor`
  - [ ] Ensure `crate::appsec::payload::body::parse_body` can correctly parse:
    - [ ] JSON
    - [ ] XML
    - [ ] URL-encoded form data
    - [ ] Multipart form data
  - [ ] Ensure `process_invocation_next` correctly identifies `body` payloads & produces the expected `AppSecContext`:
    - [ ] API Gateway V1
    - [ ] API Gateway V2 (HTTP)
    - [ ] API Gateway Websocket
    - [ ] API Gateway Lambda Authorizer (Token style)
    - [ ] API Gateway Lambda Authorizer (Request style)
    - [ ] Application Load Balancer
    - [ ] Lambda URL
    - [ ] Unsupported or uninteresting payloads (SNS, SQS, etc...)
  - [ ] Ensure `process_invocation_response` correctly processes the `body` with the provided `AppSecContext` and
        produces the expected side-effects on it.
- [ ] `crate::proxy::Interceptor`
  - [ ] Ensure it does not fail if Serverless AAP is mis-configured and fails to initialize (i.e, invalid rules are
        configured)
  - [ ] Ensure payloads are correctly interecepted, sent to the `crate::appsec::Processor`, and produce the expected
        side-effects on the resulting trace (span tags & metrics should get added).
