use tokio::sync::mpsc::{self, Receiver};
use tracing::debug;

use super::my_stats_processor::MyStatsProcessor;
use super::stats_concentrator::StatsConcentrator;

use std::sync::Arc;
use tokio::sync::Mutex;
#[derive(Clone, Copy, Default)]
pub struct StatsEvent {
    pub time: u64,
    pub dummy: u64,
}


#[allow(clippy::module_name_repetitions)]
pub struct StatsAgent {
    rx: mpsc::Receiver<StatsEvent>,
    processor: MyStatsProcessor,
}

impl StatsAgent {
    #[must_use]
    pub fn new(
        rx: Receiver<StatsEvent>,
        stats_concentrator: Arc<Mutex<StatsConcentrator>>,
    ) -> StatsAgent {
        let processor = MyStatsProcessor::new(stats_concentrator);
        StatsAgent {
            rx,
            processor,
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
