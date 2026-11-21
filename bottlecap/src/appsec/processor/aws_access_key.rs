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
    /// Returns [`None`] when the tag is off, or on a `SnapStart` restore, where
    /// `AWS_ACCESS_KEY_ID` is absent or belongs to the snapshotted environment
    /// (RFC D2): the value is then never read.
    pub(super) fn for_environment(tag_enabled: bool, init_type: Option<&str>) -> Option<Self> {
        if !tag_enabled {
            return None;
        }
        if init_type == Some(crate::tags::lambda::tags::SNAP_START_VALUE) {
            debug!("aap: SnapStart restore, aws.lambda.access_key_id will not be set");
            return None;
        }
        Some(Self::from_env())
    }

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
    fn none_when_disabled_or_snapstart() {
        assert!(AwsAccessKeyId::for_environment(false, None).is_none());
        assert!(AwsAccessKeyId::for_environment(false, Some("on-demand")).is_none());
        assert!(AwsAccessKeyId::for_environment(true, Some("snap-start")).is_none());
        assert!(AwsAccessKeyId::for_environment(true, None).is_some());
        assert!(AwsAccessKeyId::for_environment(true, Some("on-demand")).is_some());
        assert!(AwsAccessKeyId::for_environment(true, Some("provisioned-concurrency")).is_some());
        assert!(AwsAccessKeyId::for_environment(true, Some("lambda-managed-instances")).is_some());
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
