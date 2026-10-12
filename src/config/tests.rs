use std::path::Path;

use super::{ResolverConfig, default_config::resolver_options, model::sandbox_readable_dir};
use crate::resolver::Platform;
use serde_json::{Value, json};

#[test]
fn missing_values_use_declared_defaults_and_native_lists_still_decode() {
    let expected = ResolverConfig::defaults_json();
    for input in [Value::Null, json!({})] {
        assert_eq!(
            serde_json::to_value(ResolverConfig::load(input).unwrap()).unwrap(),
            expected
        );
    }
    let config = ResolverConfig::load(json!({
        "proxy": "http://127.0.0.1:8080",
        "blacklist_users": ["1001"],
        "blocked_keywords": "[\"关键词\"]",
        "future_setting": "preserved by host",
    }))
    .unwrap();
    assert_eq!(config.proxy, "http://127.0.0.1:8080");
    assert_eq!(config.blacklist_users, ["1001"]);
    assert_eq!(config.blocked_keywords, ["关键词"]);
    assert_eq!(config.live_poll_interval, expected["live_poll_interval"]);
}

#[test]
fn legacy_webpage_settings_do_not_block_loading_after_engine_removal() {
    let config = ResolverConfig::load(json!({
        "obscura_enabled": true,
        "obscura_timeout_seconds": 30,
        "obscura_settle_ms": 1000,
        "show_comments": false,
        "bili_quality": "480P",
    }))
    .unwrap();
    assert!(!config.show_comments);
    assert_eq!(config.bili_quality, "480P");
    let fields = ResolverConfig::get_schema();
    assert!(
        fields
            .iter()
            .all(|field| !field.key.starts_with("obscura_"))
    );
    let defaults = ResolverConfig::defaults_json();
    assert!(
        defaults
            .as_object()
            .unwrap()
            .keys()
            .all(|key| !key.starts_with("obscura_"))
    );
}

#[test]
fn music_uses_native_endpoints_by_default_and_keeps_optional_legacy_configuration() {
    assert!(ResolverConfig::default().ncm_api_host.is_empty());
    let config = ResolverConfig::load(json!({"ncm_api_host":"http://127.0.0.1:3000"})).unwrap();
    assert_eq!(config.ncm_api_host, "http://127.0.0.1:3000");
    assert!(
        resolver_options()
            .iter()
            .any(|option| option.value == "qqmusic")
    );
}

#[test]
fn comment_display_and_platform_quality_options_are_declared_in_default_config() {
    let old = ResolverConfig::load(json!({"bili_quality": "480P"})).unwrap();
    assert!(old.show_comments);
    assert_eq!(old.bili_quality, "480P");
    assert_eq!(old.douyin_quality, "1080P");
    assert_eq!(old.xhs_quality, "1080P");
    let configured = ResolverConfig::load(json!({
        "show_comments": false,
        "bili_quality": "720P",
        "douyin_quality": "480P",
        "xhs_quality": "360P",
    }))
    .unwrap();
    assert!(!configured.show_comments);
    assert_eq!(configured.douyin_quality, "480P");
    assert_eq!(configured.xhs_quality, "360P");
    let fields = ResolverConfig::get_schema();
    let comments = fields
        .iter()
        .find(|field| field.key == "show_comments")
        .unwrap();
    assert_eq!(comments.field_type, nuono_plugin::ConfigFieldType::Switch);
    for key in ["bili_quality", "douyin_quality", "xhs_quality"] {
        let field = fields.iter().find(|field| field.key == key).unwrap();
        assert_eq!(field.field_type, nuono_plugin::ConfigFieldType::Select);
        assert_eq!(
            field
                .options
                .as_ref()
                .unwrap()
                .iter()
                .map(|option| option.value.as_str())
                .collect::<Vec<_>>(),
            ["1080P", "720P", "480P", "360P"]
        );
    }
}

#[test]
fn explicit_invalid_values_do_not_fall_back_to_defaults() {
    for input in [
        json!(false),
        json!([]),
        json!("config"),
        json!({"render_cards": null}),
        json!({"concurrent_limit": null}),
        json!({"blacklist_users": "not a JSON array"}),
        json!({"blacklist_users": [1]}),
    ] {
        assert!(ResolverConfig::load(input.clone()).is_err(), "{input}");
    }
}

#[test]
fn schema_defaults_round_trip_without_changing_array_storage() {
    let defaults = Value::Object(
        ResolverConfig::get_schema()
            .into_iter()
            .map(|field| (field.key, field.default))
            .collect(),
    );
    assert_eq!(
        defaults.as_object().unwrap().len(),
        ResolverConfig::defaults_json().as_object().unwrap().len()
    );
    for key in [
        "disabled_resolvers",
        "blacklist_users",
        "blacklist_groups",
        "blocked_keywords",
    ] {
        assert!(defaults[key].is_string());
    }
    let parsed = ResolverConfig::load(defaults).unwrap();
    assert_eq!(
        serde_json::to_value(parsed).unwrap(),
        ResolverConfig::defaults_json()
    );
}

#[test]
fn disabled_resolver_choices_follow_the_registered_platforms() {
    let options = resolver_options();
    assert_eq!(
        options
            .iter()
            .map(|option| option.value.as_str())
            .collect::<Vec<_>>(),
        Platform::all()
            .iter()
            .map(|platform| platform.as_str())
            .collect::<Vec<_>>(),
    );
    let field = ResolverConfig::get_schema()
        .into_iter()
        .find(|field| field.key == "disabled_resolvers")
        .unwrap();
    assert_eq!(field.ui.editor.as_deref(), Some("multiselect"));
    assert_eq!(field.ui.encoding.as_deref(), Some("json_string"));
    assert!(field.ui.allow_custom);
    assert_eq!(field.options.as_ref().unwrap().len(), options.len());
    let config = ResolverConfig::load(json!({
        "disabled_resolvers": serde_json::to_string(&options.iter().map(|option| &option.value).collect::<Vec<_>>()).unwrap(),
        "blacklist_users": "[\"1001\",\"1002\"]",
        "blacklist_groups": "[\"2001\"]",
        "blocked_keywords": "[\"中文关键词\",\"spoiler\"]",
    })).unwrap();
    let native_value = serde_json::to_value(config).unwrap();
    assert_eq!(native_value["blacklist_users"], json!(["1001", "1002"]));
    assert_eq!(
        native_value["blocked_keywords"],
        json!(["中文关键词", "spoiler"])
    );
    let reparsed = ResolverConfig::load(native_value.clone()).unwrap();
    assert_eq!(serde_json::to_value(reparsed).unwrap(), native_value);
}

#[test]
fn accepts_legacy_json_string_arrays() {
    let config = ResolverConfig::load(json!({
        "disabled_resolvers": "[\"twitter\"]",
        "blacklist_users": "[\"1001\"]",
        "blacklist_groups": "[]",
        "blocked_keywords": "[\"spoiler\"]"
    }))
    .unwrap();

    assert_eq!(config.disabled_resolvers, vec!["twitter"]);
    assert_eq!(config.blacklist_users, vec!["1001"]);
    assert_eq!(config.blacklist_groups, Vec::<String>::new());
    assert_eq!(config.blocked_keywords, vec!["spoiler"]);
}

#[test]
fn retains_legacy_resolver_names_without_rewriting_their_case() {
    let config = ResolverConfig::load(json!({
        "disabled_resolvers": "[\"Twitter\",\"future_platform\"]"
    }))
    .unwrap();
    assert_eq!(
        config.disabled_resolvers,
        vec!["Twitter", "future_platform"]
    );
}

#[test]
fn media_output_dir_defaults_to_empty_and_accepts_override() {
    assert_eq!(ResolverConfig::default().media_output_dir, "");

    let config = ResolverConfig::load(json!({
        "media_output_dir": "/tmp/linkshownn-media"
    }))
    .unwrap();

    assert_eq!(config.media_output_dir, "/tmp/linkshownn-media");
}

#[test]
fn configured_media_dir_wins_over_sandbox_fallback() {
    let config = ResolverConfig {
        media_output_dir: "  /srv/onebot/shared  ".into(),
        ..ResolverConfig::default()
    };
    assert_eq!(
        config.resolve_media_dir(Path::new("/plugin/data")),
        Path::new("/srv/onebot/shared")
    );
}

#[test]
fn empty_media_dir_avoids_plugin_data_dir_when_sandbox_dir_exists() {
    // macOS 上 QQ 的沙箱读不到插件 data 目录，所以留空时不能退到那里。
    let resolved = ResolverConfig::default().resolve_media_dir(Path::new("/plugin/data"));
    match sandbox_readable_dir() {
        Some(expected) => assert_eq!(resolved, expected),
        None => assert_eq!(resolved, Path::new("/plugin/data/media")),
    }
}
