use crate::config;
use crate::tags::lambda::tags::Lambda;
use std::collections::hash_map;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct Provider {
    pub tag_provider: Arc<Tag>,
}

#[derive(Debug, Clone)]
pub enum Tag {
    Lambda(Lambda),
}

impl Provider {
    #[must_use] pub fn new(
        config: Arc<config::Config>,
        runtime: String,
        metadata: &hash_map::HashMap<String, String>,
    ) -> Self {
        match runtime.as_str() {
            "lambda" => {
                let lambda_tabs = Lambda::new_from_config(config, metadata);
                Provider {
                    tag_provider: Arc::new(Tag::Lambda(lambda_tabs)),
                }
            }
            _ => panic!("Unsupported runtime: {runtime}"),
        }
    }

    #[must_use] pub fn get_tags_vec(&self) -> Vec<String> {
        self.tag_provider.get_tags_vec()
    }

    #[must_use] pub fn get_tags_string(&self) -> String {
        self.get_tags_vec().join(",")
    }
}

trait GetTagsVec {
    fn get_tags_vec(&self) -> Vec<String>;
}

impl GetTagsVec for Tag {
    fn get_tags_vec(&self) -> Vec<String> {
        match self {
            Tag::Lambda(lambda_tags) => lambda_tags.get_tags_vec(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::collections::hash_map::HashMap;

    #[test]
    fn test_provider_new() {
        let config = Arc::new(Config {
            service: Some("test-service".to_string()),
            tags: Some("test:tag,env:test".to_string()),
            ..config::Config::default()
        });
        let mut metadata = HashMap::new();
        metadata.insert(
            "function_arn".to_string(),
            "arn:aws:lambda:us-west-2:123456789012:function:my-function".to_string(),
        );
        let provider = Provider::new(config, "lambda".to_string(), &metadata);
        assert!(provider.get_tags_string().contains("service:test-service"));
    }
}
