#![cfg(feature = "logging")]

use stratify::logging::settings::{
    ConsoleSettings, FileSettings, JsonSettings, RateLimitSettings, SamplingSettings, Settings,
};

#[test]
fn external_crate_can_fully_construct_logging_config() {
    let config = Settings::default()
        .with_level("debug")
        .with_console(ConsoleSettings {
            color: false,
            thread_ids: true,
            target: false,
            lossy: true,
            ..Default::default()
        })
        .with_json(JsonSettings {
            span_list: false,
            flatten: true,
            lossy: true,
        })
        .with_file(FileSettings {
            directory: "/var/log/example".into(),
            rotation: "hourly".into(),
            retention_days: 14,
            span_list: true,
            flatten: false,
            lossy: false,
            ..Default::default()
        })
        .with_rate_limit(RateLimitSettings {
            max_events: 500,
            per_secs: 10,
        })
        .with_sampling(SamplingSettings {
            rate: 0.25,
            min_level: "debug".into(),
        })
        .with_queue_size(32_000)
        .with_reloadable(true);

    assert_eq!(config.level.as_deref(), Some("debug"));
    assert_eq!(config.queue_size, Some(32_000));
    assert_eq!(config.reloadable, Some(true));
    assert!(config.console.as_ref().unwrap().lossy);
    assert!(!config.json.as_ref().unwrap().span_list);
    assert_eq!(config.file.as_ref().unwrap().retention_days, 14);
    assert_eq!(config.rate_limit.as_ref().unwrap().max_events, 500);
    assert_eq!(config.sampling.as_ref().unwrap().rate, 0.25);
}
