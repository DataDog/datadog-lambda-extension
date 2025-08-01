use crate::tags::lambda::tags::Lambda;
use crate::{LAMBDA_RUNTIME_SLUG, config};
use std::collections::hash_map;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct Provider {
    pub tag_provider: Arc<TagProvider>,
}

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone)]
pub enum TagProvider {
    Lambda(Lambda),
}

impl Provider {
    #[must_use]
    pub fn new(
        config: Arc<config::Config>,
        runtime: String,
        metadata: &hash_map::HashMap<String, String>,
    ) -> Self {
        match runtime.as_str() {
            LAMBDA_RUNTIME_SLUG => {
                let lambda_tags = Lambda::new_from_config(config, metadata);
                Provider {
                    tag_provider: Arc::new(TagProvider::Lambda(lambda_tags)),
                }
            }
            _ => panic!("Unsupported runtime: {runtime}"),
        }
    }

    #[must_use]
    pub fn get_tags_vec(&self) -> Vec<String> {
        self.tag_provider.get_tags_vec()
    }

    #[must_use]
    pub fn get_tags_string(&self) -> String {
        self.get_tags_vec().join(",")
    }

    #[must_use]
    pub fn get_canonical_id(&self) -> Option<String> {
        self.tag_provider.get_canonical_id()
    }

    #[must_use]
    pub fn get_canonical_resource_name(&self) -> Option<String> {
        self.tag_provider.get_canonical_resource_name()
    }

    #[must_use]
    pub fn get_tags_map(&self) -> &hash_map::HashMap<String, String> {
        self.tag_provider.get_tags_map()
    }

    #[must_use]
    pub fn get_function_tags_map(&self) -> hash_map::HashMap<String, String> {
        self.tag_provider.get_function_tags_map()
    }
}

trait GetTags {
    fn get_tags_vec(&self) -> Vec<String>;
    fn get_canonical_id(&self) -> Option<String>;
    fn get_canonical_resource_name(&self) -> Option<String>;
    fn get_tags_map(&self) -> &hash_map::HashMap<String, String>;
    fn get_function_tags_map(&self) -> hash_map::HashMap<String, String>;
}

impl GetTags for TagProvider {
    fn get_tags_vec(&self) -> Vec<String> {
        match self {
            TagProvider::Lambda(lambda_tags) => lambda_tags.get_tags_vec(),
        }
    }

    fn get_canonical_id(&self) -> Option<String> {
        match self {
            TagProvider::Lambda(lambda_tags) => lambda_tags.get_function_arn().cloned(),
        }
    }

    fn get_canonical_resource_name(&self) -> Option<String> {
        match self {
            TagProvider::Lambda(lambda_tags) => lambda_tags.get_function_name().cloned(),
        }
    }

    fn get_tags_map(&self) -> &hash_map::HashMap<String, String> {
        match self {
            TagProvider::Lambda(lambda_tags) => lambda_tags.get_tags_map(),
        }
    }

    fn get_function_tags_map(&self) -> hash_map::HashMap<String, String> {
        match self {
            TagProvider::Lambda(lambda_tags) => lambda_tags.get_function_tags_map(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LAMBDA_RUNTIME_SLUG;
    use crate::config::Config;
    use std::collections::hash_map::HashMap;

    #[test]
    fn test_provider_new() {
        let config = Arc::new(Config {
            service: Some("test-service".to_string()),
            tags: HashMap::from([
                ("test".to_string(), "tag".to_string()),
                ("env".to_string(), "test".to_string()),
            ]),
            ..config::Config::default()
        });
        let mut metadata = HashMap::new();
        metadata.insert(
            "function_arn".to_string(),
            "arn:aws:lambda:us-west-2:123456789012:function:my-function".to_string(),
        );
        let provider = Provider::new(config, LAMBDA_RUNTIME_SLUG.to_string(), &metadata);
        assert!(provider.get_tags_string().contains("service:test-service"));
    }
}
