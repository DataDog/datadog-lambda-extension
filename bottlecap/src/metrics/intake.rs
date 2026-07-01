use crate::config::Config;
use dogstatsd::datadog::{
    DdDdUrl, DdUrl, MetricsIntakeUrlPrefix, MetricsIntakeUrlPrefixOverride, Site as MetricsSite,
};
use tracing::error;

pub fn metrics_intake_url(config: &Config) -> MetricsIntakeUrlPrefix {
    if !config.dd_url.is_empty() {
        let dd_dd_url = DdDdUrl::new(config.dd_url.clone()).expect("can't parse DD_DD_URL");
        let prefix_override = MetricsIntakeUrlPrefixOverride::maybe_new(None, Some(dd_dd_url));
        MetricsIntakeUrlPrefix::new(None, prefix_override)
    } else if !config.url.is_empty() {
        let dd_url = DdUrl::new(config.url.clone()).expect("can't parse DD_URL");
        let prefix_override = MetricsIntakeUrlPrefixOverride::maybe_new(Some(dd_url), None);
        MetricsIntakeUrlPrefix::new(None, prefix_override)
    } else {
        let metrics_site = MetricsSite::new(config.site.clone()).expect("can't parse site");
        MetricsIntakeUrlPrefix::new(Some(metrics_site), None)
    }
    .expect("can't parse site or override")
}

pub struct MetricsEndpoint {
    pub intake_url: MetricsIntakeUrlPrefix,
    pub api_key: String,
}

pub fn additional_endpoints(config: &Config) -> Vec<MetricsEndpoint> {
    let mut result = Vec::new();
    for (endpoint_url, api_keys) in &config.additional_endpoints {
        let dd_url = match DdUrl::new(trim_url(endpoint_url)) {
            Ok(url) => url,
            Err(err) => {
                error!(
                    "Invalid additional endpoint: {err}. Falling back to 'https://app.datadoghq.com'"
                );
                DdUrl::new("https://app.datadoghq.com".to_string())
                    .expect("additional endpoint fallback URL is invalid")
            }
        };
        let prefix_override = MetricsIntakeUrlPrefixOverride::maybe_new(Some(dd_url), None);
        let intake_url = MetricsIntakeUrlPrefix::new(None, prefix_override)
            .expect("can't parse additional endpoint URL");
        for api_key in api_keys {
            result.push(MetricsEndpoint {
                intake_url: intake_url.clone(),
                api_key: api_key.clone(),
            });
        }
    }
    result
}

fn trim_url(url: &str) -> String {
    url.trim_end_matches('/').to_owned()
}

#[cfg(test)]
mod tests {
    use super::{additional_endpoints, metrics_intake_url};
    use crate::config::Config;
    use std::collections::HashMap;

    #[test]
    fn test_dd_dd_url_takes_priority_over_dd_url() {
        let config = Config {
            dd_url: "https://dd-dd-url.datadoghq.com".to_string(),
            url: "https://dd-url.datadoghq.com".to_string(),
            ..Config::default()
        };
        assert_eq!(
            metrics_intake_url(&config).to_string(),
            "https://dd-dd-url.datadoghq.com"
        );
    }

    #[test]
    fn test_site_fallback() {
        let config = Config {
            site: "datadoghq.com".to_string(),
            ..Config::default()
        };
        assert_eq!(
            metrics_intake_url(&config).to_string(),
            "https://api.datadoghq.com"
        );
    }

    #[test]
    fn test_additional_endpoints() {
        let config = Config {
            additional_endpoints: HashMap::from([(
                "https://dr-failover.agent.datadoghq.com".to_string(),
                vec!["api-key-1".to_string(), "api-key-2".to_string()],
            )]),
            ..Config::default()
        };
        let mut results = additional_endpoints(&config);
        results.sort_by(|a, b| a.api_key.cmp(&b.api_key));
        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0].intake_url.to_string(),
            "https://dr-failover.agent.datadoghq.com"
        );
        assert_eq!(results[0].api_key, "api-key-1");
        assert_eq!(
            results[1].intake_url.to_string(),
            "https://dr-failover.agent.datadoghq.com"
        );
        assert_eq!(results[1].api_key, "api-key-2");
    }

    #[test]
    fn test_additional_endpoints_trims_trailing_slash() {
        let config = Config {
            additional_endpoints: HashMap::from([(
                "https://dr-failover.agent.datadoghq.com/".to_string(),
                vec!["api-key-1".to_string()],
            )]),
            ..Config::default()
        };
        let results = additional_endpoints(&config);
        assert_eq!(
            results[0].intake_url.to_string(),
            "https://dr-failover.agent.datadoghq.com"
        );
    }

    #[test]
    fn test_additional_endpoints_url_fallback() {
        let config = Config {
            additional_endpoints: HashMap::from([(
                "not a valid url!!!".to_string(),
                vec!["api-key-1".to_string()],
            )]),
            ..Config::default()
        };
        let results = additional_endpoints(&config);
        assert_eq!(
            results[0].intake_url.to_string(),
            "https://app.datadoghq.com"
        );
        assert_eq!(results[0].api_key, "api-key-1");
    }
}
