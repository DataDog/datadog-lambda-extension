use sha2::{Sha256, Digest};

#[derive(Clone)]
pub struct SpanPointer {
    pub hash: String,
    pub kind: String,
}

#[must_use]
pub fn generate_span_pointer_hash(components: &[&str]) -> String {
    let data_to_hash = components.join("|");
    let mut hasher = Sha256::new();
    hasher.update(data_to_hash.as_bytes());
    let result = hasher.finalize();
    hex::encode(result)[..32].to_string()
}
