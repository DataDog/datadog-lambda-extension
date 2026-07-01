use crate::config::Config;
use dogstatsd::datadog::{
    DdDdUrl, DdUrl, MetricsIntakeUrlPrefix, MetricsIntakeUrlPrefixOverride, Site as MetricsSite,
};

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

#[cfg(test)]
mod tests {
    use super::metrics_intake_url;
    use crate::config::Config;

    #[test]
    fn dd_dd_url_takes_priority_over_dd_url() {
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
    fn falls_back_to_site() {
        let config = Config {
            site: "datadoghq.com".to_string(),
            ..Config::default()
        };
        assert_eq!(
            metrics_intake_url(&config).to_string(),
            "https://api.datadoghq.com"
        );
    }
}
