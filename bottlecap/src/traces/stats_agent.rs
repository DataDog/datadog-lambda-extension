use tokio::sync::mpsc::{self, Receiver, Sender};
use tracing::error;

use crate::traces::{stats_concentrator_service::StatsConcentratorHandle, stats_concentrator::{AggregationKey, Stats}};

#[derive(Clone, Copy)]
pub struct StatsEvent {
    pub time: u64,
    pub aggregation_key: AggregationKey,
    pub stats: Stats,
}

#[allow(clippy::module_name_repetitions)]
pub struct StatsAgent {
    tx: Sender<StatsEvent>,
    rx: Receiver<StatsEvent>,
    concentrator: StatsConcentratorHandle,
}

impl StatsAgent {
    #[must_use]
    pub fn new(concentrator: StatsConcentratorHandle) -> StatsAgent {
        let (tx, rx) = mpsc::channel::<StatsEvent>(1000);
        StatsAgent {
            tx,
            rx,
            concentrator,
        }
    }

    pub async fn spin(&mut self) {
        while let Some(event) = self.rx.recv().await {
            if let Err(e) = self.concentrator.add(event) {
                error!("Error adding stats event to the stats concentrator: {e}");
            }
        }
    }

    #[must_use]
    pub fn get_sender_copy(&self) -> Sender<StatsEvent> {
        self.tx.clone()
    }
}
