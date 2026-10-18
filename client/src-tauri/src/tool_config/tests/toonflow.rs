#[test]
fn toonflow_upsert_preserves_existing_custom_provider_and_uses_native_schema() {
    with_temp_home(|_| {
        let path = toonflow_settings_path().unwrap();
        let existing = serde_json::json!({
            "settings": {
                "customProviders": [{
                    "id": "test",
                    "label": "test",
                    "apiUrl": "http://example.invalid/v1",
                    "apiKey": "user-key",
                    "protocol": "openai-responses",
                    "models": [{"id": "user-model", "label": "User model"}]
                }]
            }
        });
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, json_text(&existing).unwrap()).unwrap();

        let models = tool_models_from_ids(&["gpt-6-luna".to_string(), "gpt-6-sol".to_string()]);
        let applied = apply_toonflow_config(
            TOOL_CONFIG_ROOT_URL,
            "secret-key",
            &models,
            ToolProtocol::OpenAiResponses,
        )
        .unwrap();
        assert!(!applied.already_configured);
        assert_eq!(applied.details.get("model_count"), Some(&"2".to_string()));

        let current = read_json_or_default(&path, serde_json::json!({})).unwrap();
        let providers = current["settings"]["customProviders"].as_array().unwrap();
        assert_eq!(providers.len(), 2);
        assert_eq!(providers[0]["id"], "test");
        let managed = providers.iter().find(|provider| provider["id"] == "const-api").unwrap();
        assert_eq!(managed["label"], "CONST API");
        assert_eq!(managed["apiUrl"], "http://127.0.0.1:38787/v1");
        assert_eq!(managed["protocol"], "openai-responses");
        assert_eq!(managed["models"][0]["label"], "gpt-6-luna");
        assert_eq!(managed["models"][0]["contextWindow"], 272000);

        assert!(check_toonflow_config(TOOL_CONFIG_ROOT_URL, "secret-key")
            .unwrap()
            .already_configured);
        remove_toonflow_config().unwrap();
        let current = read_json_or_default(&path, serde_json::json!({})).unwrap();
        let providers = current["settings"]["customProviders"].as_array().unwrap();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0]["id"], "test");
    });
}
