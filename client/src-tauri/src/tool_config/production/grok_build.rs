// Official contract: xai-org/grok-build, user-guide/11-custom-models.md.
// Verified with Grok Build 1.0.44 against a local Responses/Chat/Messages fixture.
fn grok_build_config_path() -> PathBuf {
    tool_home_override_or_default("GROK_HOME", ".grok").join("config.toml")
}

fn grok_build_backend(protocol: ToolProtocol) -> Result<&'static str> {
    match protocol {
        ToolProtocol::OpenAiResponses => Ok("responses"),
        ToolProtocol::OpenAiChat => Ok("chat_completions"),
        ToolProtocol::AnthropicMessages => Ok("messages"),
        _ => Err(anyhow!(
            "unsupported Grok Build protocol: {}",
            protocol.as_str()
        )),
    }
}

fn grok_build_protocol(backend: &str) -> Option<ToolProtocol> {
    match backend {
        "responses" => Some(ToolProtocol::OpenAiResponses),
        "chat_completions" => Some(ToolProtocol::OpenAiChat),
        "messages" => Some(ToolProtocol::AnthropicMessages),
        _ => None,
    }
}

fn grok_build_model_url(base_url: &str, protocol: ToolProtocol) -> String {
    // Grok appends only `messages`, whereas the Anthropic SDK appends `v1/messages`.
    let base = tool_surface_url(base_url, protocol.surface());
    if protocol == ToolProtocol::AnthropicMessages {
        format!("{base}/v1")
    } else {
        base
    }
}

fn configure_grok_build(
    base_url: &str,
    api_key: &str,
    models: Option<&[ToolModelInfo]>,
    fallback: ToolProtocol,
) -> Result<ToolApplyResult> {
    let path = grok_build_config_path();
    let raw = read_text_or_empty(&path)?;
    let mut doc = raw
        .parse::<DocumentMut>()
        .context("parse Grok Build config.toml")?;
    let previous = doc
        .get("models")
        .and_then(|v| v.get("default"))
        .and_then(Item::as_str)
        .unwrap_or_default()
        .to_string();
    let mut configured = false;
    let mut selected_protocol = fallback;
    if let Some(models) = models {
        let models = configured_additional_tool_models("Grok Build", models)?;
        for key in ["models", "model"] {
            if !doc.contains_key(key) {
                doc[key] = Item::Table(Table::new());
            }
            if !doc[key].is_table() {
                return Err(anyhow!("Grok Build {key} must be a table"));
            }
        }
        let entries = doc["model"]
            .as_table_mut()
            .context("Grok Build model table")?;
        let aliases = models
            .iter()
            .map(|m| format!("const-api/{}", m.id.trim()))
            .collect::<Vec<_>>();
        // Reinsert managed entries in catalog order, retaining their advanced
        // settings. Parsed TOML table positions otherwise keep the old order.
        fn reset_positions(table: &mut Table) {
            table.set_position(None);
            for (_, item) in table.iter_mut() {
                match item {
                    Item::Table(child) => reset_positions(child),
                    Item::ArrayOfTables(array) => array.iter_mut().for_each(reset_positions),
                    _ => {}
                }
            }
        }
        let mut existing = HashMap::new();
        for alias in entries
            .iter()
            .map(|(key, _)| key.to_string())
            .collect::<Vec<_>>()
        {
            if alias.starts_with("const-api/") {
                existing.insert(
                    alias.clone(),
                    entries.remove(&alias).expect("existing model"),
                );
            }
        }
        for (model, alias) in models.iter().zip(&aliases) {
            entries[alias] = existing
                .remove(alias)
                .unwrap_or_else(|| Item::Table(Table::new()));
            let entry = entries[alias]
                .as_table_mut()
                .context("Grok Build managed model must be a table")?;
            reset_positions(entry);
            let protocol = tool_model_protocol("grok-build", model, fallback);
            entry["model"] = toml_value(model.id.trim());
            entry["name"] = toml_value(if model.display_name.trim().is_empty() {
                model.id.trim()
            } else {
                model.display_name.trim()
            });
            entry["api_backend"] = toml_value(grok_build_backend(protocol)?);
            entry["base_url"] = toml_value(grok_build_model_url(base_url, protocol));
            entry["api_key"] = toml_value(api_key);
            entry.remove("auth_provider");
            entry.remove("api_base_url");
            // Grok's picker uses this menu, not a generic reasoning flag.
            // ReasoningEffort is a closed enum in the upstream Rust schema.
            let efforts = declared_reasoning_efforts(model)
                .into_iter()
                .filter(|effort| {
                    matches!(
                        effort.as_str(),
                        "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
                    )
                })
                .collect::<Vec<_>>();
            entry["supports_reasoning_effort"] = toml_value(!efforts.is_empty());
            if efforts.is_empty() {
                entry.remove("reasoning_efforts");
            } else {
                let mut menu = toml_edit::Array::new();
                for effort in efforts {
                    menu.push(effort);
                }
                entry["reasoning_efforts"] = Item::Value(toml_edit::Value::Array(menu));
            }
            for (field, value) in [
                ("context_window", model.context_tokens),
                ("max_completion_tokens", model.output_tokens),
            ] {
                if let Some(value) = value.filter(|v| *v > 0) {
                    entry[field] = toml_value(
                        i64::try_from(value).context("Grok Build model token limit exceeds i64")?,
                    );
                }
            }
        }
        let selected = if aliases.contains(&previous) {
            previous.clone()
        } else {
            aliases[0].clone()
        };
        doc["models"]["default"] = toml_value(&selected);
        // Upstream's unauthenticated summary fallback retains its built-in wire
        // model ID while borrowing our endpoint. Pin auxiliaries to a real entry.
        doc["models"]["session_summary"] = toml_value(&selected);
        doc["models"]["prompt_suggestion"] = toml_value(&selected);
    } else if let Some(entries) = doc.get_mut("model").and_then(Item::as_table_mut) {
        let mut all_valid = true;
        for (alias, item) in entries
            .iter_mut()
            .filter(|(alias, _)| alias.starts_with("const-api/"))
        {
            let Some(entry) = item.as_table_mut() else {
                all_valid = false;
                continue;
            };
            let Some(protocol) = entry
                .get("api_backend")
                .and_then(Item::as_str)
                .and_then(grok_build_protocol)
            else {
                all_valid = false;
                continue;
            };
            all_valid &= entry
                .get("model")
                .and_then(Item::as_str)
                .is_some_and(|id| !id.trim().is_empty());
            entry["base_url"] = toml_value(grok_build_model_url(base_url, protocol));
            entry["api_key"] = toml_value(api_key);
            entry.remove("auth_provider");
            entry.remove("api_base_url");
            if alias.get() == previous {
                configured = true;
                selected_protocol = protocol;
            }
        }
        configured &= all_valid;
    }
    if models.is_none() {
        configured &= ["session_summary", "prompt_suggestion"]
            .iter()
            .all(|field| {
                doc.get("models")
                    .and_then(|v| v.get(field))
                    .and_then(Item::as_str)
                    .is_some_and(|alias| {
                        alias.starts_with("const-api/")
                            && doc.get("model").and_then(|v| v.get(alias)).is_some()
                    })
            });
    }
    let mut result = ToolApplyBuilder::default();
    if models.is_some() {
        write_text_with_backup(&path, &doc.to_string(), "grok-build", &mut result)?;
    } else {
        observe_text(&path, &doc.to_string(), &mut result)?;
    }
    let mut result = result.finish("grok-build");
    if models.is_none() {
        result.already_configured &= configured;
    }
    attach_tool_protocol(&mut result, selected_protocol);
    Ok(result)
}
