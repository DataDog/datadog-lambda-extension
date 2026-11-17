//! Cost of `DD_APPSEC_AWS_ACCESS_KEY_ID_TAG_ENABLED` on one
//! monitored invocation (request, response, `aws.lambda` span), key on against off.

use std::collections::HashMap;
use std::hint::black_box;

use bottlecap::appsec::processor::Processor;
use bottlecap::config::Config;
use bottlecap::lifecycle::invocation::triggers::IdentifiedTrigger;
use bytes::Bytes;
use criterion::{Criterion, criterion_group, criterion_main};
use libdd_trace_protobuf::pb::Span;

fn processor(tag_enabled: bool) -> Processor {
    Processor::new(&Config {
        ext: bottlecap::config::LambdaConfig {
            serverless_appsec_enabled: true,
            appsec_aws_access_key_id_tag_enabled: tag_enabled,
            ..Default::default()
        },
        ..Config::default()
    })
    .expect("failed to create Processor")
}

fn bench(c: &mut Criterion) {
    // SAFETY: set once, before any other thread starts.
    unsafe { std::env::set_var("AWS_ACCESS_KEY_ID", "ASIAEXAMPLE000000001") };

    let trigger = IdentifiedTrigger::LambdaFunctionUrlEvent(
        serde_json::from_slice(include_bytes!(
            "../tests/payloads/lambda_function_url_event.json"
        ))
        .expect("failed to parse payload"),
    );
    let response = Bytes::from_static(br#"{"statusCode":200}"#);
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");

    let mut group = c.benchmark_group("appsec_monitored_invocation");
    for (name, tag_enabled) in [("key_off", false), ("key_on", true)] {
        let mut processor = processor(tag_enabled);
        group.bench_function(name, |b| {
            b.iter(|| {
                rt.block_on(async {
                    processor.process_invocation_next("rid", &trigger).await;
                    processor.process_invocation_result("rid", &response).await;
                });
                let mut span = Span {
                    name: "aws.lambda".into(),
                    meta: HashMap::from([("request_id".to_string(), "rid".to_string())]),
                    ..Default::default()
                };
                processor.process_span(&mut span);
                black_box(span)
            });
        });
    }
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(200).measurement_time(std::time::Duration::from_secs(20));
    targets = bench
}
criterion_main!(benches);
