//!Types to serialize data into the Datadog API

use datadog_protos::metrics::SketchPayload;
use protobuf::Message;
use reqwest;
use serde::{Serialize, Serializer};
use serde_json;
use tracing::{debug, error};

/// Interface for the `DogStatsD` metrics intake API.
#[derive(Debug)]
pub struct DdApi {
    api_key: String,
    fqdn_site: String,
    client: reqwest::Client,
}
/// Error relating to `ship`
#[derive(thiserror::Error, Debug)]
pub enum ShipError {
    #[error("Failed to push to API with status {status}: {body}")]
    /// Datadog API failure
    Failure {
        /// HTTP status code
        status: u16,
        /// HTTP body that failed
        body: String,
    },

    /// Json
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl DdApi {
    #[must_use]
    pub fn new(api_key: String, site: String) -> Self {
        DdApi {
            api_key,
            fqdn_site: site,
            client: reqwest::Client::new(),
        }
    }

    /// Ship a serialized series to the API, blocking
    pub async fn ship_series(&self, series: &Series) {
        let body = serde_json::to_vec(&series).expect("failed to serialize series");
        debug!("Sending body: {:?}", &series);

        let url = format!("{}/api/v2/series", &self.fqdn_site);
        let resp = self
            .client
            .post(&url)
            .header("DD-API-KEY", &self.api_key)
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await;

        match resp {
            Ok(resp) => match resp.status() {
                reqwest::StatusCode::ACCEPTED => {}
                unexpected_status_code => {
                    debug!(
                        "{}: Failed to push to API: {:?}",
                        unexpected_status_code,
                        resp.text().await.unwrap_or_default()
                    );
                }
            },
            Err(e) => {
                debug!("500: Failed to push to API: {:?}", e);
            }
        };
    }

    pub async fn ship_distributions(&self, sketches: &SketchPayload) {
        let url = format!("{}/api/beta/sketches", &self.fqdn_site);
        debug!("Sending distributions: {:?}", &sketches);
        // TODO maybe go to coded output stream if we incrementally
        // add sketch payloads to the buffer
        // something like this, but fix the utf-8 encoding issue
        // {
        //     let mut output_stream = CodedOutputStream::vec(&mut buf);
        //     let _ = output_stream.write_tag(1, protobuf::rt::WireType::LengthDelimited);
        //     let _ = output_stream.write_message_no_tag(&sketches);
        //     TODO not working, has utf-8 encoding issue in dist-intake
        //}
        let resp = self
            .client
            .post(&url)
            .header("DD-API-KEY", &self.api_key)
            .header("Content-Type", "application/x-protobuf")
            .body(sketches.write_to_bytes().expect("can't write to buffer"))
            .send()
            .await;
        match resp {
            Ok(resp) => match resp.status() {
                reqwest::StatusCode::ACCEPTED => {}
                unexpected_status_code => {
                    debug!(
                        "{}: Failed to push to API: {:?}",
                        unexpected_status_code,
                        resp.text().await.unwrap_or_default()
                    );
                }
            },
            Err(e) => {
                debug!("500: Failed to push to API: {:?}", e);
            }
        };
    }
}

#[derive(Debug, Serialize, Clone, Copy)]
/// A single point in time
pub(crate) struct Point {
    /// The time at which the point exists
    pub(crate) timestamp: u64,
    /// The point's value
    pub(crate) value: f64,
}

#[derive(Debug, Serialize)]
/// A named resource
pub(crate) struct Resource {
    /// The name of this resource
    pub(crate) name: &'static str,
    #[serde(rename = "type")]
    /// The kind of this resource
    pub(crate) kind: &'static str,
}

#[derive(Debug, Clone, Copy)]
/// The kinds of metrics the Datadog API supports
pub(crate) enum DdMetricKind {
    /// An accumulating sum
    Count,
    /// An instantaneous value
    Gauge,
}

impl Serialize for DdMetricKind {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match *self {
            DdMetricKind::Count => serializer.serialize_u32(0),
            DdMetricKind::Gauge => serializer.serialize_u32(1),
        }
    }
}

#[derive(Debug, Serialize)]
#[allow(clippy::struct_field_names)]
/// A named collection of `Point` instances.
pub(crate) struct Metric {
    /// The name of the point collection
    pub(crate) metric: &'static str,
    /// The collection of points
    pub(crate) points: [Point; 1],
    /// The resources associated with the points
    pub(crate) resources: Vec<Resource>,
    #[serde(rename = "type")]
    /// The kind of metric
    pub(crate) kind: DdMetricKind,
    pub(crate) tags: Vec<String>,
}

#[derive(Debug, Serialize)]
/// A collection of metrics as defined by the Datadog Metrics API.
// NOTE we have a number of `Vec` instances in this implementation that could
// otherwise be arrays, given that we have constants. Serializing to JSON would
// require us to avoid serializing None or Uninit values, so there's some custom
// work that's needed. For protobuf this more or less goes away.
pub struct Series {
    /// The collection itself
    pub(crate) series: Vec<Metric>,
}

impl Series {
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.series.len()
    }
}
