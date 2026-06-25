//! Data Streams Monitoring (DSM) support.
//!
//! This module provides `dd-trace-js`-compatible primitives for continuing an
//! inbound DSM pathway from request payloads and computing consume-side
//! checkpoint hashes inside the extension.
//!
//! The pieces are split so the compatibility-sensitive steps can be tested in
//! isolation:
//! * [`context`] — decode inbound pathway context (base64 + binary + varint).
//! * [`pathway`] — compute the pathway/checkpoint hash.
//! * [`checkpoint`] — compute a consume-side checkpoint from an extracted context.
//! * [`propagation_hash`] — optional process/container-tag propagation hash.

pub mod aggregator;
pub mod checkpoint;
pub mod context;
pub mod pathway;
pub mod processor;
pub mod propagation_hash;
pub mod sketch;

pub use checkpoint::{Checkpoint, compute_consume_checkpoint};
pub use context::DsmContext;
pub use pathway::compute_pathway_hash;
pub use processor::DsmProcessor;
