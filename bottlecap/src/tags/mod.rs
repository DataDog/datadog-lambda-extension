pub mod lambda;
pub mod provider;

#[must_use]
pub fn tag_should_be_dropped(tag_key: &str, tags_to_drop: &[String]) -> bool {
    tags_to_drop.iter().any(|tag_to_drop| {
        let drop_key = tag_to_drop
            .trim()
            .split_once(':')
            .map_or(tag_to_drop.trim(), |(key, _)| key.trim());
        !drop_key.is_empty() && tag_key.eq_ignore_ascii_case(drop_key)
    })
}

#[cfg(test)]
mod tests {
    use super::tag_should_be_dropped;

    #[test]
    fn test_tag_should_be_dropped() {
        let tags_to_drop = vec![
            "function_arn".to_string(),
            " executedversion ".to_string(),
            "cold_start:true".to_string(),
        ];

        assert!(tag_should_be_dropped("function_arn", &tags_to_drop));
        assert!(tag_should_be_dropped("executedversion", &tags_to_drop));
        assert!(tag_should_be_dropped("cold_start", &tags_to_drop));
        assert!(!tag_should_be_dropped("runtime", &tags_to_drop));
    }
}
