use crate::metrics::constants;
use crate::metrics::errors::ParseError;
use fnv::FnvHasher;
use std::hash::{Hash, Hasher};
use ustr::Ustr;
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
/// Determine what kind/type of a metric has come in
pub enum Type {
    /// Dogstatsd 'count' metric type, monotonically increasing counter
    Count,
    /// Dogstatsd 'gauge' metric type, point-in-time value
    Gauge,
    /// Dogstatsd 'distribution' metric type, histogram
    Distribution,
}

/// Representation of a dogstatsd Metric
///
/// For now this implementation covers only counters and gauges. We hope this is
/// enough to demonstrate the impact of this program's design goals.
#[derive(Clone, Copy, Debug)]
pub struct Metric {
    /// Name of the metric.
    ///
    /// Never more bytes than constants::MAX_METRIC_NAME_BYTES,
    /// enforced by construction. Note utf8 issues.
    pub(crate) name: Ustr,
    /// What kind/type of metric this is.
    pub(crate) kind: Type,
    /// Values of the metric. A singular value may be either a floating point or
    /// a integer. Although undocumented we assume 64 bit. A single metric may
    /// encode multiple values a time in a message. There must be at least one
    /// value here, meaning that there is guaranteed to be a Some value in the
    /// 0th index.
    ///
    /// Parsing of the values to an integer type is deferred until the last
    /// moment.
    ///
    /// Never longer than constants::MAX_VALUE_BYTES. Note utf8 issues.
    values: Ustr,
    /// Tags of the metric.
    ///
    /// The key is never longer than constants::MAX_TAG_KEY_BYTES, the value
    /// never more than constants::MAX_TAG_VALUE_BYTES. These are enforced by
    /// the parser. We assume here that tags are not sent in random order by the
    /// clien or that, if they are, the API will tidy that up. That is `a:1,b:2`
    /// is a different tagset from `b:2,a:1`.
    pub(crate) tags: Option<Ustr>,
}


impl Metric {
    /// Parse a metric from given input.
    ///
    /// This function parses a passed `&str` into a `Metric`. We assume that
    /// DogStatsD metrics must be utf8 and are not ascii or some other encoding.
    ///
    /// # Errors
    ///
    /// This function will return with an error if the input violates any of the
    /// limits in [`constants`]. Any non-viable input will be discarded.
    /// example aj-test.increment:1|c|#user:aj-test from 127.0.0.1:50983
    pub fn parse(input: &str) -> Result<Metric, crate::metrics::errors::ParseError> {
        // TODO must enforce / exploit constraints given in `constants`.
        let mut sections = input.split('|');

        let nv_section = sections
            .next()
            .ok_or(ParseError::Raw("Missing metric name and value"))?;

        let (name, values) = nv_section
            .split_once(':')
            .ok_or(ParseError::Raw("Missing name, value section"))?;

        let kind_section = sections
            .next()
            .ok_or(ParseError::Raw("Missing metric type"))?;
        let kind = match kind_section {
            "c" => Type::Count,
            "g" => Type::Gauge,
            "d" => Type::Distribution,
            _ => {
                return Err(ParseError::Raw("Unsupported metric type"));
            }
        };

        let mut tags = None;
        for section in sections {
            if section.starts_with('@') {
                // Sample rate section, skip for now.
                continue;
            }
            if let Some(tags_section) = section.strip_prefix('#') {
                let tag_parts = tags_section.split(',');
                // Validate that the tags have the right form.
                for (i, part) in tag_parts.filter(|s| !s.is_empty()).enumerate() {
                    if i >= constants::MAX_TAGS {
                        return Err(ParseError::Raw("Too many tags"));
                    }
                    if !part.contains(':') {
                        return Err(ParseError::Raw("Invalid tag format"));
                    }
                }
                tags = Some(tags_section);
                break;
            }
        }

        Ok(Metric {
            name: Ustr::from(name),
            kind,
            values: Ustr::from(values),
            tags: tags.map(Ustr::from),
        })
    }
    /// Return an iterator over values
    pub(crate) fn values(
        &self,
    ) -> impl Iterator<Item = Result<f64, std::num::ParseFloatError>> + '_ {
        self.values.split(':').map(|b: &str| {
            let num = b.parse::<f64>()?;
            Ok(num)
        })
    }

    pub(crate) fn first_value(&self) -> Result<f64, crate::metrics::errors::ParseError> {
        match self.values().next() {
            Some(v) => match v {
                Ok(v) => Ok(v),
                Err(_e) => Err(ParseError::Raw("Failed to parse value as float")),
            },
            None => Err(ParseError::Raw("No value")),
        }
    }

    pub(crate) fn tags(&self) -> Vec<String> {
        self.tags
            .unwrap_or_default()
            .to_string()
            .split(',')
            .map(|tag| tag.to_string())
            .collect()
    }

    #[cfg(test)]
    fn raw_values(&self) -> &str {
        self.values.as_str()
    }

    #[cfg(test)]
    fn raw_name(&self) -> &str {
        self.name.as_str()
    }

    #[cfg(test)]
    fn raw_tagset(&self) -> Option<&str> {
        self.tags.map(|t| t.as_str())
    }
}

/// Create an ID given a name and tagset.
///
/// This function constructs a hash of the name, the tagset and that hash is
/// identical no matter the internal order of the tagset. That is, we consider a
/// tagset like "a:1,b:2,c:3" to be idential to "b:2,c:3,a:1" to "c:3,a:1,b:2"
/// etc. This implies that we must sort the tagset after parsing it, which we
/// do. Note however that we _do not_ handle duplicate tags, so "a:1,a:1" will
/// hash to a distinct ID than "a:1".
///
/// Note also that because we take `Ustr` arguments its possible that we've
/// interned many possible combinations of a tagset, even if they are identical
/// from the point of view of this function.
#[inline]
#[must_use]
pub fn id(name: Ustr, tagset: Option<Ustr>) -> u64 {
    let mut hasher = FnvHasher::default();

    name.hash(&mut hasher);
    // We sort tags. This is in feature parity with DogStatsD and also means
    // that we avoid storing the same context multiple times because users have
    // passed tags in differeing order through time.
    if let Some(tagset) = tagset {
        let mut tag_count = 0;
        let mut scratch = [None; constants::MAX_TAGS];
        for kv in tagset.split(',') {
            if let Some((k, v)) = kv.split_once(':') {
                scratch[tag_count] = Some((Ustr::from(k), Ustr::from(v)));
                tag_count += 1;
            }
        }
        scratch[..tag_count].sort_unstable();
        // With the tags sorted -- note they're Copy -- we hash the whole kit.
        for kv in scratch[..tag_count].iter().flatten() {
            kv.0.as_bytes().hash(&mut hasher);
            kv.1.as_bytes().hash(&mut hasher);
        }
    }
    hasher.finish()
}
// <METRIC_NAME>:<VALUE>:<VALUE>:<VALUE>|<TYPE>|@<SAMPLE_RATE>|#<TAG_KEY_1>:<TAG_VALUE_1>,<TAG_KEY_2>:<TAG_VALUE_2>|c:<CONTAINER_ID>
//
// Types:
//  * c -- COUNT, allows packed values, summed
//  * g -- GAUGE, allows packed values, last one wins
//
// SAMPLE_RATE ignored for the sake of simplicity.

#[cfg(test)]
mod tests {
    use proptest::{collection, option, strategy::Strategy, string::string_regex};
    use ustr::Ustr;

    use crate::metrics::metric::id;

    use super::{Metric, ParseError};

    fn metric_name() -> impl Strategy<Value = String> {
        string_regex("[a-zA-Z0-9.-]{1,128}").unwrap()
    }

    fn metric_values() -> impl Strategy<Value = String> {
        string_regex("[0-9]+(:[0-9]){0,8}").unwrap()
    }

    fn metric_type() -> impl Strategy<Value = String> {
        string_regex("g|c").unwrap()
    }

    fn metric_tagset() -> impl Strategy<Value = Option<String>> {
        option::of(
            string_regex("[a-zA-Z]{1,64}:[a-zA-Z]{1,64}(,[a-zA-Z]{1,64}:[a-zA-Z]{1,64}){0,31}")
                .unwrap(),
        )
    }

    fn metric_tags() -> impl Strategy<Value = Vec<(String, String)>> {
        collection::vec(("[a-z]{1,8}", "[A-Z]{1,8}"), 0..32)
    }

    proptest::proptest! {
        // For any valid name, tags et al the parse routine is able to parse an
        // encoded metric line.
        #[test]
        fn parse_valid_inputs(
            name in metric_name(),
            values in metric_values(),
            mtype in metric_type(),
            tagset in metric_tagset()
        ) {
            let input = if let Some(ref tagset) = tagset {
                format!("{name}:{values}|{mtype}|#{tagset}")
            } else {
                format!("{name}:{values}|{mtype}")
            };
            let metric = Metric::parse(&input).unwrap();
            assert_eq!(name, metric.raw_name());
            assert_eq!(values, metric.raw_values());
            assert_eq!(tagset, metric.raw_tagset().map(String::from));
        }

        #[test]
        fn parse_missing_name_and_value(
            mtype in metric_type(),
            tagset in metric_tagset()
        ) {
            let input = if let Some(ref tagset) = tagset {
                format!("|{mtype}|#{tagset}")
            } else {
                format!("|{mtype}")
            };
            let result = Metric::parse(&input);

            assert!(result.is_err());
            assert_eq!(
                result.unwrap_err(),
                ParseError::Raw("Missing name, value section")
            );
        }

        #[test]
        fn parse_invalid_name_and_value_format(
            name in metric_name(),
            values in metric_values(),
            mtype in metric_type(),
            tagset in metric_tagset()
        ) {
            // If there is a ':' in the values we cannot distinguish where the
            // name and the first value are.
            let value = values.split(":").next().unwrap();
            let input = if let Some(ref tagset) = tagset {
                format!("{name}{value}|{mtype}|#{tagset}")
            } else {
                format!("{name}{value}|{mtype}")
            };
            let result = Metric::parse(&input);

            assert!(result.is_err());
            assert_eq!(
                result.unwrap_err(),
                ParseError::Raw("Missing name, value section")
            );
        }

        #[test]
        fn parse_unsupported_metric_type(
            name in metric_name(),
            values in metric_values(),
            mtype in "[abefhijklmnopqrstuvwxyz]",
            tagset in metric_tagset()
        ) {
            let input = if let Some(ref tagset) = tagset {
                format!("{name}:{values}|{mtype}|#{tagset}")
            } else {
                format!("{name}:{values}|{mtype}")
            };
            let result = Metric::parse(&input);

            assert!(result.is_err());
            assert_eq!(
                result.unwrap_err(),
                ParseError::Raw("Unsupported metric type")
            );
        }

        // The ID of a name, tagset is the same even if the tagset is in a
        // distinct order.
        // For any valid name, tags et al the parse routine is able to parse an
        // encoded metric line.
        #[test]
        fn id_consistent(name in metric_name(),
                         mut tags in metric_tags()) {
            let mut tagset1 = String::new();
            let mut tagset2 = String::new();

            for (k,v) in &tags {
                tagset1.push_str(k);
                tagset1.push_str(":");
                tagset1.push_str(v);
                tagset1.push_str(",");
            }
            tags.reverse();
            for (k,v) in &tags {
                tagset2.push_str(k);
                tagset2.push_str(":");
                tagset2.push_str(v);
                tagset2.push_str(",");
            }
            if !tags.is_empty() {
                tagset1.pop();
                tagset2.pop();
            }

            let id1 = id(Ustr::from(&name), Some(Ustr::from(&tagset1)));
            let id2 = id(Ustr::from(&name), Some(Ustr::from(&tagset2)));

            assert_eq!(id1, id2);
        }
    }

    #[test]
    fn parse_too_many_tags() {
        // 33
        assert_eq!(Metric::parse("foo:1|g|#a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3").unwrap_err(),
        ParseError::Raw("Too many tags"));

        // 32
        assert!(Metric::parse("foo:1|g|#a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2").is_ok());

        // 31
        assert!(Metric::parse("foo:1|g|#a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1").is_ok());

        // 30
        assert!(Metric::parse("foo:1|g|#a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3").is_ok());
    }
}
