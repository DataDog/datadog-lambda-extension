//! DogStatsD datagrams as a client library sends them to UDP 8125.

/// The tags bottlecap adds to every metric (`SortedTags::parse(ENRICH_TAGS)`).
pub const ENRICH_TAGS: &str = "function_arn:arn:aws:lambda:us-east-1:123456789012:function:my-checkout-fn,region:us-east-1,account_id:123456789012,aws_account:123456789012,functionname:my-checkout-fn,resource:my-checkout-fn,executedversion:$LATEST,memorysize:128,architecture:arm64,runtime:nodejs22.x,_dd.runtime_family:nodejs,env:prod,service:checkout,version:1.2.3,dd_extension_version:90,_dd.compute_stats:1";

/// A distribution with 8 tags, the heaviest common line.
pub const DIST_8_TAGS: &str = "app.checkout.latency:12.5|d|#env:prod,service:checkout,version:1.2.3,region:us-east-1,team:payments,endpoint:/api/v1/pay,status:200,method:POST";

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
    format!("my.metric:1|c|#env:prod,service:api,request_id:{i:08x}-0000-4000-8000-{i:012x}")
}

/// `n` lines joined by `\n`, one UDP datagram (keep it under 8 KiB).
pub fn packet(kind: Kind, first: usize, n: usize) -> String {
    (first..first + n)
        .map(|i| line(kind, i))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `total` lines split into datagrams of `per_packet` lines.
pub fn packets(kind: Kind, total: usize, per_packet: usize) -> Vec<String> {
    (0..total)
        .step_by(per_packet.max(1))
        .map(|first| packet(kind, first, per_packet.min(total - first)))
        .collect()
}
