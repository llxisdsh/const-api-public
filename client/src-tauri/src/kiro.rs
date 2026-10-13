//! Kiro's personal-account control plane. Catalog and quota requests never run inference.
use crate::{
    model::QuotaWindow, model_catalog::ModelObservation,
    subscription_oauth_error::SubscriptionOAuthError,
    upstream_transport::AdaptiveRequestBuilderExt,
};
use anyhow::{Context, Result, anyhow, ensure};
use futures_util::StreamExt;
use reqwest::{Client, Url};
use serde_json::{Value, json};
use std::{collections::HashSet, time::Duration};

pub(crate) mod event_stream;
pub(crate) mod request;
pub(crate) mod stream;

const AUTH: &str = "https://prod.us-east-1.auth.desktop.kiro.dev";
pub(crate) const CALLBACK: &str = "http://localhost:3128/oauth/callback";
const MAX_METADATA_BYTES: usize = 2 * 1024 * 1024;

pub(crate) fn http_client() -> Result<Client> {
    static CLIENT: std::sync::OnceLock<crate::http_client::HttpClient> = std::sync::OnceLock::new();
    Ok(crate::http_client::cached_client(&CLIENT, || {
        Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(crate::MODEL_REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
    })?)
}

pub(crate) fn validate_credential(value: &Value) -> Result<()> {
    ensure!(
        value["auth_type"].as_str() == Some("social"),
        "Kiro requires a Google or GitHub social credential"
    );
    let profile = value["profile_arn"]
        .as_str()
        .ok_or_else(|| anyhow!("Kiro credential is missing profile_arn"))?;
    profile_region(profile)?;
    ensure!(
        value["refresh_token"]
            .as_str()
            .is_some_and(|v| !v.trim().is_empty()),
        "Kiro credential is missing refresh_token"
    );
    Ok(())
}

pub(crate) fn authorization_url(state: &str, challenge: &str) -> Result<String> {
    let mut url = Url::parse("https://app.kiro.dev/signin")?;
    url.query_pairs_mut()
        .append_pair("state", state)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        // The portal adds the callback path and selected identity provider.
        .append_pair("redirect_uri", "http://localhost:3128")
        .append_pair("redirect_from", "kirocli");
    Ok(url.into())
}

pub(crate) fn callback_provider(callback: &str, state: &str) -> Result<String> {
    let url = Url::parse(callback).context("Kiro requires the complete callback URL")?;
    ensure!(
        url.scheme() == "http"
            && url.host_str() == Some("localhost")
            && url.port() == Some(3128)
            && url.path() == "/oauth/callback"
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none(),
        "Kiro callback does not match this authorization session"
    );
    let value = |key: &str| -> Result<String> {
        let values: Vec<_> = url.query_pairs().filter(|(name, _)| name == key).collect();
        ensure!(
            values.len() == 1 && !values[0].1.is_empty(),
            "Kiro callback is missing or repeats {key}"
        );
        Ok(values[0].1.to_string())
    };
    ensure!(
        !state.is_empty() && value("state")? == state,
        "OAuth state mismatch"
    );
    value("code")?;
    let provider = value("login_option")?;
    ensure!(
        matches!(provider.as_str(), "google" | "github"),
        "Kiro currently supports Google or GitHub authorization; AWS Builder ID and organization login require a different flow"
    );
    Ok(provider)
}

pub(crate) async fn exchange_authorization(
    client: &Client,
    code: &str,
    verifier: &str,
    provider: &str,
) -> Result<Value> {
    ensure!(
        matches!(provider, "google" | "github"),
        "unsupported Kiro login provider"
    );
    let body = json!({"code":code,"code_verifier":verifier,
        "redirect_uri":format!("{CALLBACK}?login_option={provider}")});
    let value = oauth_post(
        client,
        &format!("{AUTH}/oauth/token"),
        &body,
        &[code, verifier],
    )
    .await?;
    normalize_token(value, None)
}

pub(crate) async fn refresh_token(client: &Client, credential: &Value) -> Result<Value> {
    // Social credentials are distinct from Builder ID's OIDC client registration.
    ensure!(
        credential["auth_type"].as_str() == Some("social"),
        "unsupported Kiro credential type"
    );
    let token = credential["refresh_token"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("Kiro credential is missing refresh_token"))?;
    let value = oauth_post(
        client,
        &format!("{AUTH}/refreshToken"),
        &json!({"refreshToken":token}),
        &[token],
    )
    .await?;
    normalize_token(value, Some(credential))
}

async fn oauth_post(client: &Client, url: &str, body: &Value, secrets: &[&str]) -> Result<Value> {
    let response = client
        .post(url)
        .header("User-Agent", user_agent())
        .timeout(Duration::from_secs(30))
        .json(body)
        .send_adaptive()
        .await?;
    let status = response.status().as_u16();
    let value = bounded_json(response).await?;
    if !(200..300).contains(&status) {
        let message = error_detail(&value, secrets);
        let code = value["error"]
            .as_str()
            .or_else(|| value["code"].as_str())
            .unwrap_or("oauth_error");
        return Err(SubscriptionOAuthError::from_response(
            "kiro",
            status,
            &json!({"error":code,"error_description":message}),
        )
        .into());
    }
    Ok(value)
}

pub(crate) fn normalize_token(value: Value, previous: Option<&Value>) -> Result<Value> {
    let access = value["accessToken"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("Kiro token response is missing accessToken"))?;
    let refresh = value["refreshToken"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| previous.and_then(|v| v["refresh_token"].as_str()))
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("Kiro token response is missing refreshToken"))?;
    let lifetime = value["expiresIn"]
        .as_i64()
        .filter(|n| *n > 0)
        .ok_or_else(|| anyhow!("Kiro token response has no valid expiry"))?;
    let expires_at = crate::now_unix()
        .checked_add(lifetime)
        .ok_or_else(|| anyhow!("Kiro token expiry is out of range"))?;
    let profile = value["profileArn"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| previous.and_then(|v| v["profile_arn"].as_str()))
        .ok_or_else(|| anyhow!("Kiro token response is missing profileArn"))?;
    profile_region(profile)?;
    reqwest::header::HeaderValue::from_str(&format!("Bearer {access}"))
        .context("invalid Kiro access token")?;
    Ok(json!({"provider":"kiro","type":"kiro","auth_type":"social",
        "access_token":access,"refresh_token":refresh,"expires_at_unix":expires_at,
        "profile_arn":profile}))
}

pub(crate) fn profile_region(profile: &str) -> Result<&str> {
    let parts: Vec<_> = profile.split(':').collect();
    ensure!(
        parts.len() == 6
            && parts[0] == "arn"
            && parts[1] == "aws"
            && parts[2] == "codewhisperer"
            && !parts[4].is_empty()
            && parts[4].bytes().all(|b| b.is_ascii_digit())
            && parts[5]
                .strip_prefix("profile/")
                .is_some_and(|id| !id.is_empty()
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))),
        "invalid Kiro account profile"
    );
    let region = parts[3];
    let segments: Vec<_> = region.split('-').collect();
    ensure!(
        segments.len() == 3
            && segments[0].len() == 2
            && segments[0].bytes().all(|b| b.is_ascii_lowercase())
            && !segments[1].is_empty()
            && segments[1].len() <= 16
            && segments[1].bytes().all(|b| b.is_ascii_lowercase())
            && segments[2].len() == 1
            && segments[2].bytes().all(|b| b.is_ascii_digit()),
        "unsupported Kiro account region"
    );
    Ok(region)
}

fn user_agent() -> &'static str {
    concat!("const-api/", env!("CARGO_PKG_VERSION"))
}

pub(crate) fn api_request(
    client: &Client,
    url: &str,
    token: &str,
    target: &str,
    body: &Value,
) -> reqwest::RequestBuilder {
    client
        .post(url)
        .bearer_auth(token)
        .header("User-Agent", user_agent())
        .header("Content-Type", "application/x-amz-json-1.0")
        .header("x-amz-target", target)
        .header("x-amzn-codewhisperer-optout", "true")
        .json(body)
}

pub(crate) async fn catalog(
    client: &Client,
    token: &str,
    profile: &str,
) -> Result<Vec<ModelObservation>> {
    let region = profile_region(profile)?;
    let url = format!("https://management.{region}.kiro.dev/");
    let mut models = Vec::new();
    let mut cursor = None;
    let mut seen = HashSet::new();
    for _ in 0..100 {
        let mut body = json!({"origin":"AI_EDITOR","profileArn":profile});
        if let Some(cursor) = cursor {
            body["nextToken"] = Value::String(cursor);
        }
        let value = metadata_post(
            client,
            &url,
            token,
            "KiroControlPlaneBearerService.ListAvailableModels",
            &body,
        )
        .await?;
        models.extend(catalog_page(&value)?);
        match value.get("nextToken").filter(|v| !v.is_null()) {
            None => {
                ensure!(
                    !models.is_empty(),
                    "Kiro account catalog returned no models"
                );
                return Ok(crate::model_catalog::stable_dedupe_models(models));
            }
            Some(Value::String(next)) if next.is_empty() => {
                ensure!(
                    !models.is_empty(),
                    "Kiro account catalog returned no models"
                );
                return Ok(crate::model_catalog::stable_dedupe_models(models));
            }
            Some(Value::String(next)) => {
                ensure!(
                    seen.insert(next.clone()),
                    "Kiro model catalog repeated its pagination cursor"
                );
                cursor = Some(next.clone());
            }
            Some(_) => anyhow::bail!("Kiro model catalog returned an invalid pagination cursor"),
        }
    }
    anyhow::bail!("Kiro model catalog exceeded 100 pages; no partial catalog was accepted")
}

pub(crate) fn catalog_page(value: &Value) -> Result<Vec<ModelObservation>> {
    let entries = value["models"]
        .as_array()
        .ok_or_else(|| anyhow!("Kiro catalog is missing models"))?;
    entries
        .iter()
        .map(|entry| {
            let id = entry["modelId"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| anyhow!("Kiro catalog contains a model without an ID"))?;
            let limit = |key: &str| -> Result<Option<u64>> {
                match entry["tokenLimits"].get(key).filter(|v| !v.is_null()) {
                    None => Ok(None),
                    Some(value) => {
                        Ok(Some(value.as_u64().filter(|v| *v > 0).ok_or_else(
                            || anyhow!("Kiro catalog has an invalid {key}"),
                        )?))
                    }
                }
            };
            Ok(ModelObservation {
                id: id.into(),
                source: "kiro_account_catalog".into(),
                kiro_request_schema: match entry
                    .get("additionalModelRequestFieldsSchema")
                    .filter(|value| !value.is_null())
                {
                    None => None,
                    Some(value) => {
                        let schema = match value.as_str() {
                            Some(raw) => serde_json::from_str::<Value>(raw)?,
                            None => value.clone(),
                        };
                        ensure!(
                            schema.is_object(),
                            "Kiro model {id} has an invalid request schema"
                        );
                        Some(schema)
                    }
                },
                context_tokens: limit("maxInputTokens")?,
                output_tokens: limit("maxOutputTokens")?,
                input_modalities: vec!["text".into()],
                output_modalities: vec!["text".into()],
                // Credits are not USD/token prices. Do not synthesize probe_cost_nanos.
                ..Default::default()
            })
        })
        .collect()
}

pub(crate) async fn quota(client: &Client, token: &str, profile: &str) -> Result<Vec<QuotaWindow>> {
    let region = profile_region(profile)?;
    let mut url = Url::parse(&format!("https://codewhisperer.{region}.amazonaws.com/"))?;
    url.query_pairs_mut()
        .append_pair("isEmailRequired", "false")
        .append_pair("profileArn", profile);
    let value = metadata_post(
        client,
        url.as_str(),
        token,
        "AmazonCodeWhispererService.GetUsageLimits",
        &json!({"isEmailRequired":false,"profileArn":profile}),
    )
    .await?;
    quota_windows(&value, crate::now_unix())
}

pub(crate) fn quota_windows(value: &Value, now: i64) -> Result<Vec<QuotaWindow>> {
    let entries = value["usageBreakdownList"]
        .as_array()
        .ok_or_else(|| anyhow!("Kiro quota response is missing usageBreakdownList"))?;
    let mut windows = Vec::new();
    for entry in entries
        .iter()
        .filter(|e| e["resourceType"].as_str() == Some("CREDIT"))
    {
        // These pools are additive, not independent hard limits. In particular,
        // an exhausted included allowance must not hide an active trial or bonus.
        let mut limit = precise_number(entry, "usageLimit")?;
        let mut remaining = (limit - precise_number(entry, "currentUsage")?).max(0.0);
        let mut reset = timestamp(
            entry
                .get("nextDateReset")
                .or_else(|| value.get("nextDateReset")),
        )?;
        if let Some(trial) = entry.get("freeTrialInfo").filter(|v| v.is_object()) {
            let expiry = timestamp(trial.get("freeTrialExpiry"))?;
            if trial["freeTrialStatus"].as_str() == Some("ACTIVE") && expiry > now {
                let trial_limit = precise_number(trial, "usageLimit")?;
                remaining += (trial_limit - precise_number(trial, "currentUsage")?).max(0.0);
                limit += trial_limit;
                reset = if reset == 0 {
                    expiry
                } else {
                    reset.min(expiry)
                };
            }
        }
        if let Some(bonuses) = entry.get("bonuses").and_then(Value::as_array) {
            for bonus in bonuses {
                let expiry = timestamp(bonus.get("expiresAt").or_else(|| bonus.get("expiryDate")))?;
                if bonus["status"].as_str() != Some("ACTIVE") || (expiry != 0 && expiry <= now) {
                    continue;
                }
                let bonus_limit = precise_number(bonus, "usageLimit")?;
                remaining += (bonus_limit - precise_number(bonus, "currentUsage")?).max(0.0);
                limit += bonus_limit;
                if expiry > 0 {
                    reset = if reset == 0 {
                        expiry
                    } else {
                        reset.min(expiry)
                    };
                }
            }
        }
        ensure!(
            limit.is_finite() && remaining.is_finite(),
            "Kiro quota total is out of range"
        );
        let ratio = if limit > 0.0 {
            (remaining / limit).clamp(0.0, 1.0)
        } else {
            0.0
        };
        // A user-enabled overage facility is not exhausted with the included pool.
        // Keep its included balance visible as an auxiliary, non-gating window.
        let overage = value
            .pointer("/overageConfiguration/overageStatus")
            .and_then(Value::as_str)
            == Some("ENABLED");
        windows.push(QuotaWindow {
            source: "kiro_billing".into(),
            window: if overage {
                "included:monthly"
            } else {
                "monthly"
            }
            .into(),
            remaining_ratio: ratio,
            used_percent: (1.0 - ratio) * 100.0,
            reset_at_unix: reset,
            checked_at_unix: now,
            model: None,
            token_type: None,
        });
    }
    Ok(windows)
}

fn precise_number(value: &Value, name: &str) -> Result<f64> {
    value
        .get(format!("{name}WithPrecision"))
        .or_else(|| value.get(name))
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite() && *v >= 0.0)
        .ok_or_else(|| anyhow!("Kiro quota has no valid {name}"))
}

fn timestamp(value: Option<&Value>) -> Result<i64> {
    match value.filter(|v| !v.is_null()) {
        None => Ok(0),
        Some(value) => value
            .as_f64()
            .filter(|v| v.is_finite() && *v >= 0.0 && *v < i64::MAX as f64)
            .map(|v| v as i64)
            .ok_or_else(|| anyhow!("Kiro quota has an invalid timestamp")),
    }
}

#[derive(Debug)]
pub(crate) struct ApiError {
    pub(crate) status: u16,
    detail: String,
}
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Kiro upstream returned HTTP {}: {}",
            self.status, self.detail
        )
    }
}
impl std::error::Error for ApiError {}

async fn metadata_post(
    client: &Client,
    url: &str,
    token: &str,
    target: &str,
    body: &Value,
) -> Result<Value> {
    let response = api_request(client, url, token, target, body)
        .timeout(Duration::from_secs(30))
        .send_adaptive()
        .await?;
    let status = response.status().as_u16();
    let value = bounded_json(response).await?;
    ensure!(
        (200..300).contains(&status),
        ApiError {
            status,
            detail: error_detail(&value, &[token])
        }
    );
    Ok(value)
}

pub(crate) async fn rejection(response: reqwest::Response, token: &str) -> Result<ApiError> {
    let status = response.status().as_u16();
    let value = bounded_json(response).await?;
    Ok(ApiError {
        status,
        detail: error_detail(&value, &[token]),
    })
}

fn error_detail(value: &Value, secrets: &[&str]) -> String {
    let mut detail = value["error_description"]
        .as_str()
        .or_else(|| value["message"].as_str())
        .or_else(|| value["__type"].as_str())
        .unwrap_or("request rejected")
        .to_string();
    for secret in secrets.iter().filter(|s| !s.is_empty()) {
        detail = detail.replace(secret, "[redacted]");
    }
    crate::upstream_failure::safe_excerpt(&detail)
}

async fn bounded_json(response: reqwest::Response) -> Result<Value> {
    let success = response.status().is_success();
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        ensure!(
            body.len().saturating_add(chunk.len()) <= MAX_METADATA_BYTES,
            "Kiro metadata exceeds 2 MiB"
        );
        body.extend_from_slice(&chunk);
    }
    match serde_json::from_slice(&body) {
        Ok(value) => Ok(value),
        Err(_) if !success => Ok(json!({"message":"non-JSON rejection body omitted"})),
        Err(error) => Err(error).context("Kiro returned invalid JSON metadata"),
    }
}

#[cfg(test)]
mod tests;
