use crate::traces::stats_agent::StatsEvent;
use tracing::{debug};

use std::sync::Arc;
use crate::traces::stats_concentrator::StatsConcentrator;
use tokio::sync::Mutex;

pub struct MyStatsProcessor {
    concentrator: Arc<Mutex<StatsConcentrator>>,
}

impl MyStatsProcessor {
    #[must_use]
    pub fn new(stats_concentrator: Arc<Mutex<StatsConcentrator>>) -> Self {
        Self { concentrator: stats_concentrator }
    }

    #[allow(clippy::cast_possible_truncation)]
    pub async fn process(
        &self,
        // TODO: get time, duration and hit/error from stats event
        event: StatsEvent,
    ) {
        debug!("In my stats processor: Processing stats event.");
        
        self.concentrator.lock().await.add(event);
    }
}