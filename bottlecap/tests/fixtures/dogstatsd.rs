//! DogStatsD datagrams as a client library sends them to UDP 8125.

use bottlecap::tags::lambda::tags::EXTENSION_VERSION;

use crate::fixtures::invocation::{
    ACCOUNT_ID, ARCHITECTURE, ENV, FUNCTION_ARN, FUNCTION_NAME, INIT_TYPE, MEMORY_SIZE_MB, REGION,
    SERVICE, VERSION,
};

/// The tags bottlecap adds to every metric of the `$LATEST` version of [`FUNCTION_ARN`]
/// (`SortedTags::parse(&enrich_tags())`). No `runtime` tag: Lambda does not give
/// `AWS_EXECUTION_ENV` to extensions.
pub fn enrich_tags() -> String {
    format!(
        "function_arn:{FUNCTION_ARN},region:{REGION},account_id:{ACCOUNT_ID},aws_account:{ACCOUNT_ID},functionname:{FUNCTION_NAME},resource:{FUNCTION_NAME},memorysize:{MEMORY_SIZE_MB},architecture:{ARCHITECTURE},init_type:{INIT_TYPE},env:{ENV},service:{SERVICE},version:{VERSION},dd_extension_version:{EXTENSION_VERSION}"
    )
}

/// A distribution with 8 tags, the heaviest common line. Its `env`, `service`, `version` and
/// `region` tags are those of the shared invocation.
pub const DIST_8_TAGS: &str = "app.checkout.latency:12.5|d|#env:prod,service:orders,version:1.2.3,region:us-east-1,team:payments,endpoint:/api/v1/pay,status:200,method:POST";

/// A count with 2 tags, the lightest common line.
pub const COUNT_2_TAGS: &str = "my.metric:1|c|#tag1:a,tag2:b";

/// Lines bottlecap drops before parsing: service checks, events, and its own enhanced metric.
pub const SKIPPED_LINES: [&str; 3] = [
    "_sc|my.check|0|#env:prod",
    "_e{5,4}:title|text",
    "aws.lambda.enhanced.invocations:1|c",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Count,
    Gauge,
    Distribution,
}

/// One metric line. The `endpoint` tag cycles over 100 values and the name moves to the next of
/// 50 values every 100 lines, so a stream of `n` lines has `min(n, 5000)` contexts.
pub fn line(kind: Kind, i: usize) -> String {
    match kind {
        Kind::Count => format!(
            "app.request.count{}:1|c|#endpoint:/api/v{},status:200,method:GET",
            (i / 100) % 50,
            i % 100
        ),
        Kind::Gauge => format!(
            "app.queue.depth{}:{}|g|#endpoint:/api/v{},status:200,method:GET",
            (i / 100) % 50,
            i % 900,
            i % 100
        ),
        Kind::Distribution => format!(
            "app.request.latency{}:{}|d|#endpoint:/api/v{},status:200,method:GET",
            (i / 100) % 50,
            (i % 900) + 1,
            i % 100
        ),
    }
}

/// A line whose tag value is unique per `i` (one new interned string per line).
pub fn unique_tag_line(i: usize) -> String {
    format!("my.metric:1|c|#env:{ENV},service:{SERVICE},request_id:{i:08x}-0000-4000-8000-{i:012x}")
}

/// `n` lines joined by `\n`, one UDP datagram (keep it under 8 KiB).
pub fn packet(kind: Kind, first: usize, n: usize) -> String {
    (first..first + n)
        .map(|i| line(kind, i))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `total` lines split into datagrams of `per_packet` lines (at least 1).
pub fn packets(kind: Kind, total: usize, per_packet: usize) -> Vec<String> {
    let per_packet = per_packet.max(1);
    (0..total)
        .step_by(per_packet)
        .map(|first| packet(kind, first, per_packet.min(total - first)))
        .collect()
}
