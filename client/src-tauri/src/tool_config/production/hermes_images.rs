// Hermes' OpenAI plugin always offers editing alongside generation. Both routes
// must be advertised for the same model, including when it has a quality tier.
pub(crate) fn hermes_image_model_ids(payload: &serde_json::Value) -> Vec<&str> {
    let mut ids = crate::image_models::catalog_models(payload)
        .into_iter()
        .flatten()
        .filter(|model| model.operations.generate && model.operations.edit)
        .map(|model| model.id)
        .collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();
    ids
}

pub(crate) fn hermes_supports_named_images(config: &Path) -> bool {
    // The official installers place the runtime under the Hermes home. A
    // different profile's version cannot establish which plugin this one runs.
    let install = config.with_file_name("hermes-agent");
    let stamp = fs::read(install.join("install-stamp.json"))
        .ok()
        .and_then(|raw| {
            let json = raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&raw);
            serde_json::from_slice::<serde_json::Value>(json).ok()
        });
    let version = stamp
        .as_ref()
        .and_then(|stamp| stamp["baseVersion"].as_str())
        .and_then(|version| semver::Version::parse(version).ok());
    version.is_some_and(|v| v >= semver::Version::new(0, 21, 6))
        && install
            .join("plugins/image_gen/openai/__init__.py")
            .is_file()
}

fn hermes_image_env_preference(config: &Path, root: &serde_yaml::Mapping) -> bool {
    // With no selected provider Hermes uses its legacy FAL settings.
    let using_default = root
        .get("image_gen")
        .and_then(|image| image["provider"].as_str())
        .is_none_or(|name| name.trim().is_empty());
    let relevant = |key: &str| {
        key == "OPENAI_IMAGE_MODEL"
            || (using_default && matches!(key, "FAL_KEY" | "FAL_IMAGE_MODEL"))
    };
    if !test_home_active()
        && ["OPENAI_IMAGE_MODEL", "FAL_KEY", "FAL_IMAGE_MODEL"]
            .iter()
            .any(|key| {
                relevant(key) && std::env::var_os(key).is_some_and(|value| !value.is_empty())
            })
    {
        return true;
    }
    // Hermes loads its profile .env before resolving the image model. A model
    // override can take precedence over the scoped YAML ID verified above.
    match fs::read_to_string(config.with_file_name(".env")) {
        Ok(raw) => raw.lines().any(|line| {
            let line = line.trim_start_matches('\u{feff}').trim_start();
            let line = line
                .strip_prefix("export")
                .and_then(|rest| rest.strip_prefix(char::is_whitespace))
                .unwrap_or(line);
            line.split_once('=').is_some_and(|(key, value)| {
                relevant(key.trim().trim_matches('\''))
                    && !matches!(value.trim(), "" | "\"\"" | "''")
            })
        }),
        Err(err) => err.kind() != std::io::ErrorKind::NotFound,
    }
}

fn hermes_image_wire_model(model: &str) -> &str {
    // These are virtual quality choices in Hermes' openai plugin. It sends
    // every other scoped model ID verbatim, without a synthetic quality value.
    match model.rsplit_once('-') {
        Some(("gpt-image-2", "low" | "medium" | "high")) => "gpt-image-2",
        Some((
            base @ ("gpt-image-2.5-flare" | "gpt-image-2.5-sunburst"),
            "low" | "medium" | "high" | "xhigh" | "max",
        )) => base,
        _ => model,
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct HermesImageSettings {
    provider: String,
    model: String,
    openai: HermesImageEndpoint,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct HermesImageEndpoint {
    provider: String,
    model: String,
    base_url: String,
    key_env: String,
}

impl HermesImageEndpoint {
    fn has_independent_route(&self) -> bool {
        let name = self.provider.trim();
        (!name.is_empty() && !hermes_const_image_name(name))
            || !self.base_url.trim().is_empty()
            || !self.key_env.trim().is_empty()
    }
}

fn hermes_switched_openai_endpoint(document: &serde_json::Value) -> bool {
    serde_json::from_value::<HermesImageSettings>(document["image_gen"].clone())
        .is_ok_and(|image| image.provider == "openai" && image.openai.has_independent_route())
}

impl HermesImageSettings {
    fn for_const(root: &serde_yaml::Mapping) -> Option<Self> {
        // Hermes resolves keyed providers (including their display names)
        // before custom_providers. A shadowed name could send the wrong key.
        if root
            .get("providers")
            .and_then(serde_yaml::Value::as_mapping)
            .is_some_and(|providers| {
                providers.iter().any(|(key, entry)| {
                    [key.as_str(), entry["name"].as_str()]
                        .into_iter()
                        .flatten()
                        .any(hermes_const_image_name)
                })
            })
        {
            return None;
        }
        let image = root.get("image_gen")?;
        let settings: Self = serde_yaml::from_value(image.clone()).ok()?;
        // Adding or removing a skill never selects a native image provider.
        // Only refresh a binding that the profile still explicitly selects.
        if settings.provider.trim() != "openai"
            || settings.openai.provider.trim() != CODEX_CONST_API_PROVIDER_ID
            || settings.openai.has_independent_route()
        {
            return None;
        }
        Some(settings)
    }

    fn select_model<'a>(&'a self, ids: &'a [&str]) -> Option<&'a str> {
        let scoped = self.openai.model.trim();
        let shared = self.model.trim();
        let choice = match (scoped, shared) {
            ("", "") => match ids {
                [only] => *only,
                _ => return None,
            },
            ("", explicit) | (explicit, "") => explicit,
            (scoped, shared) if scoped == shared => scoped,
            _ => return None,
        };
        ids.contains(&hermes_image_wire_model(choice))
            .then_some(choice)
    }
}

fn hermes_const_image_name(name: &str) -> bool {
    let name = name.trim();
    name.eq_ignore_ascii_case(CODEX_CONST_API_PROVIDER_ID)
        || name.eq_ignore_ascii_case(HERMES_CONST_API_PROVIDER_ID)
}

fn upsert_hermes_images(root: &mut serde_yaml::Mapping, ids: &[&str]) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let Some(settings) = HermesImageSettings::for_const(root) else {
        return Ok(());
    };
    if let Some(model) = settings.select_model(ids) {
        let image = ensure_yaml_mapping(root, "image_gen")?;
        let openai = ensure_yaml_mapping(image, "openai")?;
        openai.insert(yaml_key("model"), yaml_string(model));
    }
    Ok(())
}

fn hermes_image_config_complete(root: &serde_yaml::Mapping) -> bool {
    HermesImageSettings::for_const(root).is_none_or(|settings| !settings.openai.model.trim().is_empty())
}

pub(crate) fn hermes_needs_image_model_discovery() -> Result<bool> {
    for path in hermes_config_paths() {
        if hermes_supports_named_images(&path) {
            let root = parse_hermes_config(&read_text_or_empty(&path)?)?;
            if !hermes_image_config_complete(&root) && !hermes_image_env_preference(&path, &root) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}
