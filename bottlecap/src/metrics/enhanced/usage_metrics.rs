use tokio::sync::{mpsc, oneshot, watch};
use tracing::{debug, error};

#[derive(Debug, Copy, Clone, PartialEq)]
pub struct UsageMetrics {
    pub tmp_used: f64,
    pub fd_use: f64,
    pub threads_use: f64,
}

impl UsageMetrics {
    #[must_use]
    pub fn new() -> Self {
        Self {
            tmp_used: 0.0,
            fd_use: 0.0,
            threads_use: 0.0,
        }
    }

    pub fn update(&mut self, tmp_used: Option<f64>, fd_use: Option<f64>, threads_use: Option<f64>) {
        if let Some(tmp_used) = tmp_used
            && tmp_used > self.tmp_used
        {
            self.tmp_used = tmp_used;
        }
        if let Some(fd_use) = fd_use
            && fd_use > self.fd_use
        {
            self.fd_use = fd_use;
        }
        if let Some(threads_use) = threads_use
            && threads_use > self.threads_use
        {
            self.threads_use = threads_use;
        }
    }

    pub fn reset(&mut self) {
        self.tmp_used = 0.0;
        self.fd_use = 0.0;
        self.threads_use = 0.0;
    }
}

impl Default for UsageMetrics {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub enum Command {
    Update(Option<f64>, Option<f64>, Option<f64>),
    Reset(),
    Get(oneshot::Sender<UsageMetrics>),
    Shutdown,
}

#[derive(Clone, Debug)]
pub struct EnhancedMetricsHandle {
    tx: mpsc::UnboundedSender<Command>,
    monitoring_state_tx: watch::Sender<bool>, // true = active, false = paused
}

impl EnhancedMetricsHandle {
    pub fn update_metrics(
        &self,
        tmp_used: Option<f64>,
        fd_use: Option<f64>,
        threads_use: Option<f64>,
    ) -> Result<(), mpsc::error::SendError<Command>> {
        self.tx.send(Command::Update(tmp_used, fd_use, threads_use))
    }

    pub fn reset_metrics(&self) -> Result<(), mpsc::error::SendError<Command>> {
        self.tx.send(Command::Reset())
    }

    pub async fn get_metrics(&self) -> Result<UsageMetrics, String> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(Command::Get(response_tx))
            .map_err(|e| format!("Failed to send enhanced metrics command: {e}"))?;
        response_rx
            .await
            .map_err(|e| format!("Failed to receive enhanced metrics response: {e}"))
    }

    pub fn pause_monitoring(&self) {
        let _ = self
            .monitoring_state_tx
            .send(false)
            .map_err(|e| format!("Failed to pause enhanced metrics monitoring: {e}"));
    }

    pub fn resume_monitoring(&self) {
        let _ = self
            .monitoring_state_tx
            .send(true)
            .map_err(|e| format!("Failed to resume enhanced metrics monitoring: {e}"));
    }

    #[must_use]
    /// Get a receiver for monitoring state changes (true = active, false = paused)
    pub fn get_monitoring_state_receiver(&self) -> watch::Receiver<bool> {
        self.monitoring_state_tx.subscribe()
    }

    pub fn shutdown(&self) -> Result<(), mpsc::error::SendError<Command>> {
        self.tx.send(Command::Shutdown)
    }
}

pub struct EnhancedMetricsService {
    metrics: UsageMetrics,
    rx: mpsc::UnboundedReceiver<Command>,
}

impl EnhancedMetricsService {
    #[must_use]
    pub fn new() -> (Self, EnhancedMetricsHandle) {
        let metrics = UsageMetrics::new();
        let (tx, rx) = mpsc::unbounded_channel();
        let (monitoring_state_tx, _monitoring_state_rx) = watch::channel(false); // Start paused

        let service = Self { metrics, rx };
        let handle = EnhancedMetricsHandle {
            tx,
            monitoring_state_tx,
        };

        (service, handle)
    }
}

impl EnhancedMetricsService {
    pub async fn run(mut self) {
        debug!("Enhanced metrics service started - monitoring usage metrics");

        while let Some(command) = self.rx.recv().await {
            match command {
                Command::Update(tmp_used, fd_use, threads_use) => {
                    self.metrics.update(tmp_used, fd_use, threads_use);
                }
                Command::Reset() => {
                    self.metrics.reset();
                }
                Command::Get(response_tx) => {
                    if response_tx.send(self.metrics).is_err() {
                        error!("Failed to send enhanced metrics response");
                    }
                }
                Command::Shutdown => {
                    debug!("Enhanced metrics service shutting down");
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_enhanced_metrics_service() {
        let (service, handle) = EnhancedMetricsService::new();

        let service_handle = tokio::spawn(async move {
            service.run().await;
        });

        let metrics = handle.get_metrics().await.unwrap();
        assert_eq!(metrics, UsageMetrics::new());

        let res = handle.update_metrics(Some(11.0), Some(20.0), Some(5.0));
        assert!(res.is_ok());
        let metrics = handle.get_metrics().await.unwrap();
        assert_eq!(
            metrics,
            UsageMetrics {
                tmp_used: 11.0,
                fd_use: 20.0,
                threads_use: 5.0
            }
        );

        // Test that metrics are only updated if they are greater than the current value
        let res = handle.update_metrics(Some(10.0), Some(10.0), Some(2.0));
        assert!(res.is_ok());
        let metrics = handle.get_metrics().await.unwrap();
        assert_eq!(
            metrics,
            UsageMetrics {
                tmp_used: 11.0,
                fd_use: 20.0,
                threads_use: 5.0
            }
        );

        // Test pause/resume functionality (affects monitoring loop, not service)
        handle.pause_monitoring();
        handle.resume_monitoring();

        // The service itself always processes updates - pause/resume only affects the monitoring loop
        let res = handle.update_metrics(Some(50.0), Some(60.0), Some(70.0));
        assert!(res.is_ok());
        let metrics = handle.get_metrics().await.unwrap();
        assert_eq!(
            metrics,
            UsageMetrics {
                tmp_used: 50.0,
                fd_use: 60.0,
                threads_use: 70.0
            }
        );

        // Test reset metrics
        handle.reset_metrics().unwrap();
        let metrics = handle.get_metrics().await.unwrap();
        assert_eq!(metrics, UsageMetrics::new());

        handle.shutdown().unwrap();
        service_handle.await.unwrap();
    }
}
