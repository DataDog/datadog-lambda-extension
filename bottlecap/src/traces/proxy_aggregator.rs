use bytes::Bytes;
use reqwest::header::HeaderMap;

pub struct ProxyRequest {
    pub headers: HeaderMap,
    pub body: Bytes,
    pub target_url: String,
}

pub struct Aggregator {
    queue: Vec<ProxyRequest>,
}

impl Default for Aggregator {
    fn default() -> Self {
        Aggregator {
            queue: Vec::with_capacity(128), // arbritrary capacity for request queue
        }
    }
}

impl Aggregator {
    pub fn add(&mut self, request: ProxyRequest) {
        self.queue.push(request);
    }

    pub fn get_batch(&mut self) -> Vec<ProxyRequest> {
        std::mem::take(&mut self.queue)
    }
}
