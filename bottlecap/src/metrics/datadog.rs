//!Types to serialize data into the Datadog API

use std::env;

use ureq;
use serde::{Serialize, Serializer};
use serde_json;
use tracing::error;

/// Interface for the DogStatsD metrics intake API.
#[derive(Debug)]
pub struct DdApi {
    // runtime: tokio::runtime::Runtime,
    api_key: String,
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

impl Default for DdApi {
    fn default() -> Self {
        Self {
            api_key: env::var("DD_API_KEY").expect("unable to read envvars, catastrophic failure"),
        }
    }
}

impl DdApi {
    /// Create a new `DdApi` instance
    ///
    /// # Panics
    ///
    /// Function will panic if `DD_API_KEY` cannot be read from the environment.
    #[must_use]
    pub fn new() -> Self {
        DdApi::default()
    }

    /// Ship a serialized series to the API, blocking
    pub fn ship(&self, series: &Series) -> Result<(), ShipError> {
        // call inner_ship onto the runtime
        let api_key = self.api_key.clone();
        let body = serde_json::to_vec(&series)?;
        log::info!("sending body: {:?}", &series);

        let resp: Result<ureq::Response, ureq::Error> = ureq::post("https://api.datadoghq.com/api/v2/series")
            .set("DD-API-KEY", &api_key)
            .set("Content-Type", "application/json")
            .send_bytes(body.as_slice());
        match resp {
            Ok(_resp) => Ok(()),
            Err(ureq::Error::Status(code, response)) => {
                Err(ShipError::Failure { status: code, body: response.into_string().unwrap() })
            }
            Err(e) => { Err(ShipError::Failure { status: 500, body: e.to_string() }) }
        }
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
    /// A distribution of values
    Distribution,
}

impl Serialize for DdMetricKind {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match *self {
            DdMetricKind::Count => serializer.serialize_u32(0),
            DdMetricKind::Gauge => serializer.serialize_u32(1),
            DdMetricKind::Distribution => serializer.serialize_u32(2),
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
