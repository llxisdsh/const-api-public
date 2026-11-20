//! Model-specific contracts for the two portable OpenAI Images operations.
//! Image output alone also describes chat tools and Gemini generateContent.
use crate::{
    model::ChannelConfig,
    source_driver::{self, ExecutionKind, SourceDriverId, SubscriptionResponseMode},
    surface::{ApiOperation, ApiSurface},
};

pub(crate) const GENERATE: &str = "openai.images_generations";
pub(crate) const EDIT: &str = "openai.images_edits";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ImageOperations {
    pub(crate) generate: bool,
    pub(crate) edit: bool,
}

impl ImageOperations {
    pub(crate) fn from_json(value: &serde_json::Value) -> Option<Self> {
        let mut ops = Self::default();
        for id in value.as_array()? {
            ops.add_id(id.as_str()?);
        }
        Some(ops)
    }

    pub(crate) fn from_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Self {
        let mut ops = Self::default();
        for name in names {
            ops.add_id(match name.trim() {
                "/images/generations" | "/v1/images/generations" => GENERATE,
                "/images/edits" | "/v1/images/edits" => EDIT,
                id => id,
            });
        }
        ops
    }

    fn add_id(&mut self, id: &str) {
        match id {
            GENERATE => self.generate = true,
            EDIT => self.edit = true,
            _ => {}
        }
    }

    pub(crate) fn from_catalog_entry(entry: &serde_json::Value) -> Option<Self> {
        if let Some(ids) = entry.pointer("/const_api/image_operations") {
            return Self::from_json(ids);
        }
        let endpoints = entry.get("supported_endpoints")?.as_array()?;
        endpoints
            .iter()
            .all(serde_json::Value::is_string)
            .then(|| Self::from_names(endpoints.iter().filter_map(serde_json::Value::as_str)))
    }
    pub(crate) fn union(&mut self, other: Self) {
        self.generate |= other.generate;
        self.edit |= other.edit;
    }

    pub(crate) fn names(self) -> impl Iterator<Item = &'static str> {
        [self.generate.then_some(GENERATE), self.edit.then_some(EDIT)]
            .into_iter()
            .flatten()
    }

    fn allows(self, operation: ApiOperation) -> bool {
        match operation {
            ApiOperation::ImagesGenerations => self.generate,
            ApiOperation::ImagesEdits => self.edit,
            _ => false,
        }
    }
}

pub(crate) struct CatalogImageModel<'a> {
    pub(crate) id: &'a str,
    pub(crate) operations: ImageOperations,
    pub(crate) platform_pricing: Option<&'a serde_json::Value>,
}

// The compact transport can include hidden entries so merged catalogs can
// apply display policy. Use wire IDs verbatim for both native tools and CLI.
pub(crate) fn catalog_models(
    payload: &serde_json::Value,
) -> Option<impl Iterator<Item = CatalogImageModel<'_>>> {
    Some(visible_catalog_entries(payload)?.filter_map(|(id, entry)| {
        let operations = ImageOperations::from_json(entry.pointer("/const_api/image_operations")?)?;
        (operations.generate || operations.edit).then(|| CatalogImageModel {
            id,
            operations,
            platform_pricing: entry
                .pointer("/const_api/platform_image_pricing")
                .filter(|p| p.is_object()),
        })
    }))
}

pub(crate) fn visible_catalog_entries(
    payload: &serde_json::Value,
) -> Option<impl Iterator<Item = (&str, &serde_json::Value)>> {
    let entries = payload["data"].as_array()?;
    let policy = crate::model_discovery::ModelPresentation::from_payload(payload);
    Some(entries.iter().filter_map(move |entry| {
        let id = entry["id"].as_str()?;
        let policy = policy
            .as_ref()
            .unwrap_or_else(|| crate::model_discovery::ModelPresentation::packaged());
        (!id.is_empty() && id == id.trim() && !policy.rank(&crate::config::public_model_name(id)).1)
            .then_some((id, entry))
    }))
}

#[derive(Default)]
struct ModelDeclaration {
    operations: Option<ImageOperations>,
    dedicated_image: bool,
}

pub(crate) struct ChannelImages {
    driver: SourceDriverId,
    admitted: ImageOperations,
    declarations: std::collections::HashMap<String, ModelDeclaration>,
}

impl ChannelImages {
    pub(crate) fn new(channel: &ChannelConfig) -> Self {
        let driver = channel.source_driver();
        let admitted = match crate::channel_executor::execution_kind_for_source(driver) {
            Ok(ExecutionKind::HttpSurface) => {
                let bound =
                    crate::openrouter::supports_operation(driver, ApiOperation::ImagesGenerations)
                        && crate::channel_surface::channel_surface_target(
                            channel,
                            ApiSurface::OpenAi,
                            None,
                            true,
                        )
                        .is_some();
                ImageOperations {
                    generate: bound,
                    edit: bound,
                }
            }
            Ok(_) => {
                let admits = |op| {
                    source_driver::admit_subscription_operation(
                        driver,
                        ApiSurface::OpenAi,
                        op,
                        None,
                        SubscriptionResponseMode::Buffered,
                    )
                    .is_ok()
                };
                ImageOperations {
                    generate: admits(ApiOperation::ImagesGenerations),
                    edit: admits(ApiOperation::ImagesEdits),
                }
            }
            Err(_) => ImageOperations::default(),
        };
        let mut declarations = std::collections::HashMap::<String, ModelDeclaration>::new();
        if admitted.generate || admitted.edit {
            for profile in &channel.capability_profiles {
                let dedicated_image =
                    profile.image_output && profile.catalog_text_output == Some(false);
                let model = profile.model_pattern.trim();
                if (profile.catalog_image_operations.is_none() && !dedicated_image)
                    || model.is_empty()
                    || model.contains(['*', '?'])
                    || !matches!(profile.verification_state.as_str(), "verified" | "declared")
                    || matches!(profile.release_status.as_str(), "prepared" | "suspended")
                    || !matches!(profile.capability_layer.as_str(), "" | "model_wire")
                {
                    continue;
                }
                let entry = declarations.entry(model.to_ascii_lowercase()).or_default();
                if let Some(ops) = profile.catalog_image_operations {
                    entry.operations.get_or_insert_default().union(ops);
                }
                entry.dedicated_image |= dedicated_image;
            }
        }
        Self {
            driver,
            admitted,
            declarations,
        }
    }

    /// None is unknown (including older custom gateways). It can still be
    /// requested explicitly. Only a positive result is published for discovery.
    pub(crate) fn model(&self, id: &str) -> Option<ImageOperations> {
        if self.admitted == ImageOperations::default() {
            return Some(self.admitted);
        }
        let id = id.trim().to_ascii_lowercase();
        let declaration = self.declarations.get(&id);
        let operations = declaration.and_then(|d| d.operations).or_else(|| {
            let both = ImageOperations {
                generate: true,
                edit: true,
            };
            let generate = ImageOperations {
                generate: true,
                edit: false,
            };
            use SourceDriverId::*;
            match self.driver {
                OpenAiSubscription
                    if id == "gpt-image-2" && declaration.is_some_and(|d| d.dedicated_image) =>
                {
                    Some(both)
                }
                OpenAiApi | AzureOpenAi if id.starts_with("gpt-image-") || id == "dall-e-2" => {
                    Some(both)
                }
                OpenAiApi | AzureOpenAi if id == "dall-e-3" => Some(generate),
                XaiApi if id.starts_with("grok-imagine-image") => Some(both),
                GrokSubscription
                    if id.starts_with("grok-imagine-image")
                        && declaration.is_some_and(|d| d.dedicated_image) =>
                {
                    Some(both)
                }
                XaiApi if id == "grok-2-image" || id.starts_with("grok-2-image-") => Some(generate),
                TogetherApi | SiliconflowApi | VolcengineArkApi
                    if declaration.is_some_and(|d| d.dedicated_image) =>
                {
                    Some(generate)
                }
                // OpenRouter needs an exact account declaration for its Images API.
                // An output modality or an old ID-only snapshot is insufficient.
                Openrouter => Some(ImageOperations::default()),
                _ => None,
            }
        })?;
        Some(ImageOperations {
            generate: operations.generate && self.admitted.generate,
            edit: operations.edit && self.admitted.edit,
        })
    }

    pub(crate) fn supports(&self, id: &str, operation: ApiOperation) -> Option<bool> {
        self.model(id).map(|ops| ops.allows(operation))
    }
}
