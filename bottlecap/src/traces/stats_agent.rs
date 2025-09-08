use tokio::sync::mpsc::{self, Receiver, Sender};
use tracing::debug;

use datadog_trace_protobuf::pb;

use super::my_stats_processor::MyStatsProcessor;

use crate::config::Config;
use std::sync::Arc;
use crate::tags::provider::Provider as TagProvider;
use crate::traces::stats_aggregator::StatsAggregator;
use tokio::sync::Mutex;
#[derive(Clone, Copy)]
pub struct StatsEvent;


#[allow(clippy::module_name_repetitions)]
pub struct StatsAgent {
    rx: mpsc::Receiver<StatsEvent>,
    processor: MyStatsProcessor,
}

impl StatsAgent {
    #[must_use]
    pub fn new(
        rx: Receiver<StatsEvent>,
        config: Arc<Config>,
        tags_provider: Arc<TagProvider>,
        stats_aggregator: Arc<Mutex<StatsAggregator>>,
    ) -> StatsAgent {
        StatsAgent {
            rx,
            processor: MyStatsProcessor::new(config, tags_provider, stats_aggregator),
        }
    }

    pub async fn spin(&mut self) {
        while let Some(event) = self.rx.recv().await {
            debug!("In stats agent: Received stats event.");
            self.processor.process(event).await;
        }
    }

    // pub async fn sync_consume(&mut self) {
    //     if let Some(events) = self.rx.recv().await {
    //         self.processor.process().await;
    //     }
    // }

    // #[must_use]
    // pub fn get_sender_copy(&self) -> Sender<StatsEvent> {
    //     self.tx.clone()
    // }

}
