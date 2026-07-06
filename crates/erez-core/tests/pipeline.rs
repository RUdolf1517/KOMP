use erez_core::{
    normalize::matches_wake_phrase, Action, DefaultIntentResolver, ErezConfig, IntentRequest,
    IntentResolver, PluginRegistry,
};
use std::path::PathBuf;

#[tokio::test]
async fn wake_phrase_is_accepted() {
    let config = ErezConfig::default();
    assert!(matches_wake_phrase(
        "комп, открой браузер",
        &config.wake_grammar
    ));
}

#[tokio::test]
async fn non_wake_speech_is_ignored() {
    let config = ErezConfig::default();
    assert!(!matches_wake_phrase("открой браузер", &config.wake_grammar));
}

#[tokio::test]
async fn russian_command_resolves_from_plugin() {
    let registry = PluginRegistry::load_dir(example_plugins_dir()).unwrap();
    let mut config = ErezConfig::default();
    config.lmstudio.enabled = false;
    let resolver = DefaultIntentResolver::new(registry, config.lmstudio);
    let result = resolver
        .resolve(IntentRequest {
            utterance: "открой браузер".into(),
            locale_hint: Some("ru".into()),
        })
        .await
        .unwrap();
    assert_eq!(
        result.resolved.unwrap().command_id.as_deref(),
        Some("open_browser")
    );
}

#[tokio::test]
async fn short_browser_variants_resolve_from_plugin() {
    let registry = PluginRegistry::load_dir(example_plugins_dir()).unwrap();
    let mut config = ErezConfig::default();
    config.lmstudio.enabled = false;
    let resolver = DefaultIntentResolver::new(registry, config.lmstudio);

    for utterance in ["браузер", "мой браузер", "вы браузер", "открой броуди"]
    {
        let result = resolver
            .resolve(IntentRequest {
                utterance: utterance.into(),
                locale_hint: Some("ru".into()),
            })
            .await
            .unwrap();
        assert_eq!(
            result.resolved.unwrap().command_id.as_deref(),
            Some("open_browser"),
            "utterance: {utterance}"
        );
    }
}

#[tokio::test]
async fn english_command_resolves_from_plugin() {
    let registry = PluginRegistry::load_dir(example_plugins_dir()).unwrap();
    let mut config = ErezConfig::default();
    config.lmstudio.enabled = false;
    let resolver = DefaultIntentResolver::new(registry, config.lmstudio);
    let result = resolver
        .resolve(IntentRequest {
            utterance: "open browser".into(),
            locale_hint: Some("en".into()),
        })
        .await
        .unwrap();
    assert_eq!(
        result.resolved.unwrap().command_id.as_deref(),
        Some("open_browser")
    );
}

#[tokio::test]
async fn search_command_resolves_to_url_action_with_query_slot() {
    let registry = PluginRegistry::load_dir(example_plugins_dir()).unwrap();
    let mut config = ErezConfig::default();
    config.lmstudio.enabled = false;
    let resolver = DefaultIntentResolver::new(registry, config.lmstudio);
    let result = resolver
        .resolve(IntentRequest {
            utterance: "найди что-нибудь".into(),
            locale_hint: Some("ru".into()),
        })
        .await
        .unwrap();
    let resolved = result.resolved.unwrap();
    assert_eq!(resolved.command_id.as_deref(), Some("search_web"));
    assert_eq!(resolved.slots.get("query"), Some(&"что нибудь".to_string()));
    assert!(matches!(resolved.action, Action::Url { .. }));
}

#[tokio::test]
async fn unknown_command_reports_lmstudio_fallback_when_disabled() {
    let registry = PluginRegistry::load_dir(example_plugins_dir()).unwrap();
    let mut config = ErezConfig::default();
    config.lmstudio.enabled = false;
    let resolver = DefaultIntentResolver::new(registry, config.lmstudio);
    let result = resolver
        .resolve(IntentRequest {
            utterance: "что-нибудь неизвестное".into(),
            locale_hint: Some("ru".into()),
        })
        .await
        .unwrap();
    assert!(result.resolved.is_none());
    assert_eq!(result.fallback_error.as_deref(), Some("lmstudio disabled"));
}

fn example_plugins_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins.example")
}
