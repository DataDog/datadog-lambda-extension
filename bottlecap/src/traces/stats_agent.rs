use tokio::sync::mpsc::{self, Receiver, Sender};

use super::stats_concentrator::StatsConcentrator;

use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone, Copy, Default)]
pub struct StatsEvent {
    pub time: u64,
}

#[allow(clippy::module_name_repetitions)]
pub struct StatsAgent {
    tx: Sender<StatsEvent>,
    rx: Receiver<StatsEvent>,
    concentrator: Arc<Mutex<StatsConcentrator>>,
}

impl StatsAgent {
    #[must_use]
    pub fn new(
        concentrator: Arc<Mutex<StatsConcentrator>>,
    ) -> StatsAgent {
        let (tx, rx) = mpsc::channel::<StatsEvent>(1000);
        StatsAgent { tx, rx, concentrator }
    }

    pub async fn spin(&mut self) {
        while let Some(event) = self.rx.recv().await {
            self.concentrator.lock().await.add(event);
        }
    }

    #[must_use]
    pub fn get_sender_copy(&self) -> Sender<StatsEvent> {
        self.tx.clone()
    }
}
