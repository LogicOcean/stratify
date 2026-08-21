file_source!(JsonSource, "json", serde_json::from_str);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::source::test_helpers::write_temp;
    use crate::config::source::Source;

    #[tokio::test]
    async fn loads_valid_json() {
        let f = write_temp(r#"{"host": "localhost", "port": 5432}"#);
        let source = JsonSource::new(f.path(), 0);
        let val = source.load().await.unwrap();
        assert_eq!(val["host"], "localhost");
        assert_eq!(val["port"], 5432);
    }

    #[tokio::test]
    async fn missing_file_is_error() {
        let source = JsonSource::new("/nonexistent/path.json", 0);
        assert!(source.load().await.is_err());
    }

    #[tokio::test]
    async fn invalid_json_is_error() {
        let f = write_temp("{not valid json");
        let source = JsonSource::new(f.path(), 0);
        assert!(source.load().await.is_err());
    }
}
