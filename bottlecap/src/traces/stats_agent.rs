use tokio::sync::mpsc::{self, Receiver};
use tracing::debug;

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
    concentrator: Arc<Mutex<StatsConcentrator>>,
}

impl StatsAgent {
    #[must_use]
    pub fn new(
        rx: Receiver<StatsEvent>,
        concentrator: Arc<Mutex<StatsConcentrator>>,
    ) -> StatsAgent {
        StatsAgent {
            rx,
            concentrator,
        }
    }

    pub async fn spin(&mut self) {
        while let Some(event) = self.rx.recv().await {
            debug!("In stats agent: Received stats event.");
            self.concentrator.lock().await.add(event);
        }
    }
}
