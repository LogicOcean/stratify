use stratify::config::Builder;

#[tokio::test]
async fn end_to_end_multiple_sources() {
    let base = tempfile::NamedTempFile::new().unwrap();
    let overrides = tempfile::NamedTempFile::new().unwrap();

    std::fs::write(
        base.path(),
        r#"{"host": "localhost", "port": 5432, "debug": false}"#,
    )
    .unwrap();
    std::fs::write(
        overrides.path(),
        r#"{"host": "prod.example.com", "debug": true}"#,
    )
    .unwrap();

    let store = Builder::default()
        .json(base.path(), 100)
        .json(overrides.path(), 50)
        .build()
        .await
        .unwrap();

    assert_eq!(store.get_str("host").unwrap(), "prod.example.com");
    assert_eq!(store.get_u64("port").unwrap(), 5432);
    assert!(store.get_bool("debug").unwrap());
}

#[tokio::test]
async fn deep_merge_nested_objects() {
    let base = tempfile::NamedTempFile::new().unwrap();
    let overrides = tempfile::NamedTempFile::new().unwrap();

    std::fs::write(
        base.path(),
        r#"{"db": {"host": "localhost", "port": 5432, "pool": 10}}"#,
    )
    .unwrap();
    std::fs::write(
        overrides.path(),
        r#"{"db": {"host": "db.prod.local", "tls": true}}"#,
    )
    .unwrap();

    let store = Builder::default()
        .json(base.path(), 100)
        .json(overrides.path(), 50)
        .build()
        .await
        .unwrap();

    assert_eq!(store.get_str("db.host").unwrap(), "db.prod.local");
    assert_eq!(store.get_u64("db.port").unwrap(), 5432);
    assert_eq!(store.get_u64("db.pool").unwrap(), 10);
    assert!(store.get_bool("db.tls").unwrap());
}

#[tokio::test]
async fn deserialize_struct() {
    use serde::Deserialize;

    #[derive(Deserialize, Debug, PartialEq)]
    struct AppConfig {
        name: String,
        log_level: String,
    }

    let f = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        f.path(),
        r#"{"app": {"name": "hermes", "log_level": "debug"}}"#,
    )
    .unwrap();

    let store = Builder::default().json(f.path(), 0).build().await.unwrap();

    let cfg: AppConfig = store.get("app").unwrap();
    assert_eq!(cfg.name, "hermes");
    assert_eq!(cfg.log_level, "debug");
}

#[tokio::test]
async fn missing_key_is_none() {
    let f = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(f.path(), r#"{"exists": true}"#).unwrap();

    let store = Builder::default().json(f.path(), 0).build().await.unwrap();

    assert!(store.get_str("missing").is_none());
    assert!(store.get_u64("missing").is_none());
    assert!(store.get_bool("missing").is_none());
}

#[tokio::test]
async fn empty_builder_produces_empty_store() {
    let store = Builder::default().build().await.unwrap();
    assert!(store.get_str("anything").is_none());
}
