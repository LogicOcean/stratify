file_source!(YamlSource, "yaml", serde_norway::from_str);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::source::test_helpers::write_temp;
    use crate::config::source::Source;

    #[tokio::test]
    async fn loads_valid_yaml() {
        let f = write_temp("host: localhost\nport: 5432");
        let source = YamlSource::new(f.path(), 0);
        let val = source.load().await.unwrap();
        assert_eq!(val["host"], "localhost");
        assert_eq!(val["port"], 5432);
    }

    #[tokio::test]
    async fn missing_file_is_error() {
        let source = YamlSource::new("/nonexistent/path.yaml", 0);
        assert!(source.load().await.is_err());
    }

    #[tokio::test]
    async fn invalid_yaml_is_error() {
        let f = write_temp(": bad yaml");
        let source = YamlSource::new(f.path(), 0);
        assert!(source.load().await.is_err());
    }
}
