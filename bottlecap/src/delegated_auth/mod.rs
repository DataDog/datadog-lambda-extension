//! AWS Delegated Authentication module
//!
//! This module provides the ability to obtain a managed Datadog API key using
//! AWS Lambda execution role credentials via SigV4-signed STS requests.

pub mod auth_proof;
pub mod client;

pub use auth_proof::generate_auth_proof;
pub use client::get_delegated_api_key;
