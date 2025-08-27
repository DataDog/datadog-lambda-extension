## App & API Protection

This module centralizes the bulk of the Serverless App & API Protection features.
This only supports monitoring mode (no blocking), for Threat Detection and API Security.

### Enabling the feature
The functionality is typically enabled via environment variables by setting `DD_SERVERLESS_APPSEC_ENABLED=1`. This is
not to be confused with `DD_APPSEC_ENABLED=1` which activates the feature on the tracer side (depending on tracer
support).

The `AWS_LAMBDA_EXEC_WRAPPER` environment variable must also be set to `/opt/datadog_wrapper` so that the extension is
able to act as an AWS Lambda Runtime API proxy.

Finally, the Datadog language-specific layer for the function's runtime must be correctly installed.

### Configuration

Several aspects of the App & API Protection feature can be tweaked via dedicated environment variables:

Variable                       | Default        | Description
-------------------------------|----------------|-----------------------------------------------------------------------
`DD_APPSEC_RULES`              | built-in rules | Path to a local file containing a JSON-encoded WAF ruleset
`DD_APPSEC_WAF_TIMEOUT`        | `5000` (5 ms)  | The WAF execution time budget per request, in microseconds
`DD_API_SECURITY_ENABLED`      | `true`         | Whether API Secruity schema collection is enabled
`DD_API_SECURITY_SAMPLE_DELAY` | `30.0`         | The delay in seconds between two samples for API Security schemas

> [!TIP]
> AWS Lambda allocates vCPUs based on the provisionned RAM. On functions with very small RAM allocations (especially at
> the default of 128 MiB), the default `DD_APPSEC_WAF_TIMEOUT` of 5 ms may be too short for the App & API Protection
> feature to be effective. Consider increasing the RAM allocation or setting `DD_APPSEC_WAF_TIMEOUT` to a larger value.

### Internals

When App & API Protection is enabled, the `bottlecap::proxy::interceptor` module is activated, proxying the AWS Lambda
Runtime API endpoint. This allows the `bottlecap::appsec::processor::Processor` to be notified about incoming request
payloads via `Processor::process_invocation_next`, and subsequently the response via
`Processor::process_invocation_result`.

When `Processor::process_invocation_next` observes a payload that is supported by Serverless App & API Protection (that
includes API Gateway Lambda integration payloads, Application Load Balancer Lambda target payloads, etc...), it creates
a new `bottlecap::appsec::processor::context::Context` to hold security-relevant data during the lifetime of the
request; which can subsequently be used to annotate the `aws.lambda` span with security information.

Finally, the `bottlecap::traces::trace_processor::SendingTraceProcessor` intercepts _traces_ as they are prepared to be
aggregated and sent to the backend. It looks for `aws.lambda` spans and sends them to the `Processor::process_span`
method, which will add the relevant security information to the span, and:
- if `Processor::process_invocation_result` was already called for this request, the span is deemed finalized and
  immediately flushed to the aggregator;
- otherwise, the trace is witheld and flushing will resume once `Processor::process_invocation_result` is called.
