use std::cell::OnceCell;

use tracing::debug;

/// The execution environment's AWS access key ID, read on first use and
/// cached for the life of the extension process.
///
/// The first read happens on the first monitored invocation, so on `SnapStart`
/// it always happens after restore.
pub(super) struct AwsAccessKeyId {
    read: fn() -> Option<String>,
    value: OnceCell<Option<String>>,
}
impl AwsAccessKeyId {
    pub(super) fn from_env() -> Self {
        Self::with_reader(crate::config::aws::aws_access_key_id_from_env)
    }

    pub(super) const fn with_reader(read: fn() -> Option<String>) -> Self {
        Self {
            read,
            value: OnceCell::new(),
        }
    }

    pub(super) fn get(&self) -> Option<&str> {
        self.value
            .get_or_init(|| {
                let value = (self.read)();
                if value.is_none() {
                    debug!(
                        "aap: AWS_ACCESS_KEY_ID is missing or malformed, aws.lambda.access_key_id will not be set"
                    );
                }
                value
            })
            .as_deref()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn reads_once() {
        static READS: AtomicUsize = AtomicUsize::new(0);
        let subject = AwsAccessKeyId::with_reader(|| {
            READS.fetch_add(1, Ordering::SeqCst);
            Some("ASIAEXAMPLE000000001".to_string())
        });
        assert_eq!(
            READS.load(Ordering::SeqCst),
            0,
            "must not read before first use"
        );
        assert_eq!(subject.get(), Some("ASIAEXAMPLE000000001"));
        assert_eq!(subject.get(), Some("ASIAEXAMPLE000000001"));
        assert_eq!(READS.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn caches_absence() {
        static READS: AtomicUsize = AtomicUsize::new(0);
        let subject = AwsAccessKeyId::with_reader(|| {
            READS.fetch_add(1, Ordering::SeqCst);
            None
        });
        assert_eq!(subject.get(), None);
        assert_eq!(subject.get(), None);
        assert_eq!(READS.load(Ordering::SeqCst), 1);
    }
}
