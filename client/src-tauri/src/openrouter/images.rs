//! The OpenRouter Images API has its own directory, reference input and usage schema.
use crate::image_models::ImageOperations;
use crate::model_catalog::ModelObservation;
use crate::surface::ApiOperation;
use anyhow::{Result, anyhow, ensure};
use bytes::Bytes;
use serde_json::{Value, json};
use std::collections::HashMap;

pub(super) fn apply_directory(models: &mut [ModelObservation], directory: &Value) {
    let entries: HashMap<_, _> = directory["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| Some((entry["id"].as_str()?, entry)))
        .collect();
    for model in models {
        let Some(entry) = entries.get(model.id.as_str()) else {
            continue;
        };
        if model.image_operations == Some(ImageOperations::default())
            || !model.output_modalities.iter().any(|v| v == "image")
        {
            continue;
        }
        let references = &entry["supported_parameters"]["input_references"];
        let declares = |field: &str, kind: &str| {
            entry["architecture"][field]
                .as_array()
                .is_some_and(|values| values.iter().any(|value| value == kind))
        };
        if !declares("output_modalities", "image") {
            continue;
        }
        let accepts = |kind: &str| {
            model.input_modalities.iter().any(|v| v == kind) && declares("input_modalities", kind)
        };
        model.image_operations = Some(ImageOperations {
            generate: accepts("text") && references["min"].as_u64().unwrap_or(0) == 0,
            edit: accepts("image") && references["max"].as_u64().is_some_and(|n| n > 0),
        });
    }
}

pub(crate) async fn request_body(
    operation: ApiOperation,
    headers: &reqwest::header::HeaderMap,
    body: Bytes,
    wire_model: &str,
) -> Result<Bytes> {
    // Reuse the gateway's Images multipart decoder, including ordered uploads
    // and masks. File IDs and masks cannot be expressed by input_references.
    let normalized = if operation == ApiOperation::ImagesEdits {
        crate::supplier::normalize_openai_subscription_image_payload("images/edits", headers, body)
            .await?
            .body
    } else {
        body
    };
    let mut fields: serde_json::Map<String, Value> = serde_json::from_slice(&normalized)?;
    ensure!(
        fields
            .get("prompt")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty()),
        "OpenRouter Images requires a prompt"
    );
    ensure!(
        fields.remove("stream").is_none_or(|v| v == false),
        "OpenRouter Images adapter supports buffered responses"
    );
    ensure!(
        !fields.contains_key("mask"),
        "OpenRouter Images does not accept edit masks"
    );
    ensure!(
        !fields.contains_key("input_references"),
        "use the image edits endpoint with image or images for reference images"
    );
    if operation == ApiOperation::ImagesGenerations {
        ensure!(
            !fields.contains_key("image") && !fields.contains_key("images"),
            "use the image edits endpoint for reference images"
        );
    }
    if operation == ApiOperation::ImagesEdits {
        let singular = fields.remove("image");
        let plural = fields.remove("images");
        ensure!(
            singular.is_none() || plural.is_none(),
            "choose image or images for an edit"
        );
        let inputs = match singular.or(plural) {
            Some(Value::Array(items)) => items,
            Some(item) => vec![item],
            None => Vec::new(),
        };
        ensure!(
            !inputs.is_empty(),
            "OpenRouter image edits require a reference image"
        );
        let references = inputs
            .into_iter()
            .map(|input| {
                let url = match input {
                    Value::String(url) => Some(url),
                    Value::Object(mut obj) if !obj.contains_key("file_id") => {
                        match obj.remove("image_url").or_else(|| obj.remove("url")) {
                            Some(Value::String(url)) => Some(url),
                            Some(Value::Object(mut image)) => image
                                .remove("url")
                                .and_then(|v| v.as_str().map(str::to_owned)),
                            _ => None,
                        }
                    }
                    _ => None,
                }
                .ok_or_else(|| anyhow!("OpenRouter expects image URLs or uploads, not file IDs"))?;
                ensure!(
                    url.starts_with("https://")
                        || url.starts_with("http://")
                        || url.starts_with("data:image/"),
                    "invalid OpenRouter reference image"
                );
                Ok(json!({"type": "image_url", "image_url": {"url": url}}))
            })
            .collect::<Result<Vec<_>>>()?;
        fields.insert("input_references".into(), Value::Array(references));
    }
    fields.insert("model".into(), json!(wire_model));
    if fields.get("size").is_some_and(|s| s == "auto") {
        fields.remove("size");
    }
    Ok(Bytes::from(serde_json::to_vec(&fields)?))
}
