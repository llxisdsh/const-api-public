use super::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const CONTRACT_JSON: &str = include_str!("../../../shared/api-surface-contract.json");

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SurfaceContract {
    schema_version: u32,
    manifest_version: u32,
    execution_contexts: Vec<ExecutionContextContract>,
    conversion_levels: Vec<String>,
    maturity_levels: Vec<String>,
    surfaces: Vec<SurfaceContractEntry>,
    official_families: Vec<OfficialFamilyContract>,
    operations: Vec<OperationContract>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ExecutionContextContract {
    id: String,
    short: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SurfaceContractEntry {
    id: String,
    mount: String,
    api_versions: Vec<String>,
    protocols: Vec<String>,
    authentication: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct OfficialFamilyContract {
    id: String,
    surface: String,
    name: String,
    scope: String,
    priority: String,
    official_paths: Vec<String>,
    typed_operation_ids: Vec<String>,
    contexts: BTreeMap<String, FamilyContextContract>,
    official_source: String,
    verified_on: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FamilyContextContract {
    coverage: String,
    conversion: String,
    maturity: String,
    evidence: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct OperationContract {
    id: String,
    surface: String,
    method: String,
    path: String,
    protocol: Option<String>,
    transport: String,
    request_body_mode: String,
    response_modes: Vec<String>,
    ownership: String,
    statefulness: String,
    billing_class: String,
    execution_policy: String,
    required_context_declarations: Vec<String>,
    required_conversion_evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OperationSnapshot {
    id: String,
    surface: String,
    method: String,
    path: String,
    protocol: Option<String>,
    transport: String,
    ownership: String,
    statefulness: String,
    billing_class: String,
    request_body_mode: String,
    response_modes: Vec<String>,
    execution_policy: String,
    required_context_declarations: Vec<String>,
    required_conversion_evidence: Vec<String>,
}

fn parse_contract() -> SurfaceContract {
    serde_json::from_str(CONTRACT_JSON).expect("api surface contract must be valid strict JSON")
}

fn serialized_name<T: Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .expect("surface registry enum must serialize")
        .as_str()
        .expect("surface registry enum must serialize to a string")
        .to_string()
}

fn contract_snapshot(operation: &OperationContract) -> OperationSnapshot {
    OperationSnapshot {
        id: operation.id.clone(),
        surface: operation.surface.clone(),
        method: operation.method.clone(),
        path: operation.path.clone(),
        protocol: operation.protocol.clone(),
        transport: operation.transport.clone(),
        ownership: operation.ownership.clone(),
        statefulness: operation.statefulness.clone(),
        billing_class: operation.billing_class.clone(),
        request_body_mode: operation.request_body_mode.clone(),
        response_modes: operation.response_modes.clone(),
        execution_policy: operation.execution_policy.clone(),
        required_context_declarations: operation.required_context_declarations.clone(),
        required_conversion_evidence: operation.required_conversion_evidence.clone(),
    }
}

fn registry_snapshot(spec: RouteSpec) -> OperationSnapshot {
    OperationSnapshot {
        id: spec.operation_id(),
        surface: spec.surface.as_str().to_string(),
        method: spec.method.to_string(),
        path: spec.path_template.to_string(),
        protocol: spec.protocol.map(|protocol| protocol.as_str().to_string()),
        transport: serialized_name(spec.transport),
        ownership: serialized_name(spec.ownership),
        statefulness: serialized_name(spec.statefulness),
        billing_class: serialized_name(spec.billing_class),
        request_body_mode: spec.request_body_mode().to_string(),
        response_modes: spec
            .response_modes()
            .iter()
            .map(|mode| (*mode).to_string())
            .collect(),
        execution_policy: spec.execution_policy().to_string(),
        required_context_declarations: spec
            .required_context_declarations()
            .iter()
            .map(|context| (*context).to_string())
            .collect(),
        required_conversion_evidence: spec
            .required_conversion_evidence()
            .iter()
            .map(|evidence| (*evidence).to_string())
            .collect(),
    }
}

fn contract_operation_map(
    contract: &SurfaceContract,
) -> Result<BTreeMap<String, OperationSnapshot>, String> {
    let mut operations = BTreeMap::new();
    for operation in &contract.operations {
        if operations
            .insert(operation.id.clone(), contract_snapshot(operation))
            .is_some()
        {
            return Err(format!("duplicate operation id {}", operation.id));
        }
    }
    Ok(operations)
}

fn registry_operation_map() -> Result<BTreeMap<String, OperationSnapshot>, String> {
    let mut operations = BTreeMap::new();
    for spec in ROUTE_SPECS.iter().copied() {
        let snapshot = registry_snapshot(spec);
        if operations.insert(snapshot.id.clone(), snapshot).is_some() {
            return Err(format!(
                "duplicate Rust operation id {}",
                spec.operation_id()
            ));
        }
    }
    Ok(operations)
}

fn contract_matches_registry(contract: &SurfaceContract) -> Result<(), String> {
    let contract_operations = contract_operation_map(contract)?;
    let registry_operations = registry_operation_map()?;
    if contract_operations != registry_operations {
        return Err(format!(
            "operation contract and Rust registry differ\ncontract={contract_operations:#?}\nregistry={registry_operations:#?}"
        ));
    }
    Ok(())
}

fn validate_contract(contract: &SurfaceContract) -> Result<(), String> {
    if contract.schema_version != 2 {
        return Err(format!(
            "unsupported contract schema version {}",
            contract.schema_version
        ));
    }
    if contract.manifest_version != 3 {
        return Err(format!(
            "unexpected public manifest version {}",
            contract.manifest_version
        ));
    }

    let contexts = contract
        .execution_contexts
        .iter()
        .map(|context| (context.id.as_str(), context.short.as_str()))
        .collect::<BTreeSet<_>>();
    let expected_contexts = BTreeSet::from([
        ("api_credential", "A"),
        ("platform_supply", "P"),
        ("subscription_driver", "S"),
    ]);
    if contexts != expected_contexts {
        return Err(format!("execution contexts={contexts:?}"));
    }

    let conversion_levels = contract
        .conversion_levels
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if conversion_levels != BTreeSet::from(["c0", "c1", "c2", "c3"]) {
        return Err(format!("conversion levels={conversion_levels:?}"));
    }
    let maturity_levels = contract
        .maturity_levels
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if maturity_levels != BTreeSet::from(["l0", "l1", "l2", "l3", "l4", "l5", "l6"]) {
        return Err(format!("maturity levels={maturity_levels:?}"));
    }

    let mut surfaces = BTreeMap::new();
    for surface in &contract.surfaces {
        if surface.mount.is_empty()
            || surface.api_versions.is_empty()
            || surface.protocols.is_empty()
            || surface.authentication.is_empty()
        {
            return Err(format!("incomplete surface metadata {}", surface.id));
        }
        if surfaces.insert(surface.id.as_str(), surface).is_some() {
            return Err(format!("duplicate surface id {}", surface.id));
        }
    }
    if surfaces.keys().copied().collect::<BTreeSet<_>>()
        != BTreeSet::from(["anthropic", "gemini", "openai"])
    {
        return Err(format!("unexpected surfaces={:?}", surfaces.keys()));
    }

    let allowed_contexts =
        BTreeSet::from(["api_credential", "subscription_driver", "platform_supply"]);
    let allowed_evidence = BTreeSet::from(["request", "response", "stream", "error", "cancel"]);
    let operation_surfaces = contract
        .operations
        .iter()
        .map(|operation| (operation.id.as_str(), operation.surface.as_str()))
        .collect::<BTreeMap<_, _>>();
    let mut family_ids = BTreeSet::new();
    let mut assigned_operations = BTreeMap::new();
    let mut family_surfaces = BTreeSet::new();
    for family in &contract.official_families {
        let surface = surfaces
            .get(family.surface.as_str())
            .ok_or_else(|| format!("family {} references unknown surface", family.id))?;
        if family.id.is_empty()
            || !family.id.starts_with(&format!("{}.", family.surface))
            || family.name.is_empty()
            || family.official_paths.is_empty()
            || family.official_source.is_empty()
            || family.verified_on.is_empty()
            || !family_ids.insert(family.id.as_str())
        {
            return Err(format!(
                "invalid or duplicate official family {}",
                family.id
            ));
        }
        family_surfaces.insert(family.surface.as_str());
        if !matches!(family.scope.as_str(), "model_api" | "management_api")
            || !matches!(
                family.priority.as_str(),
                "p0_tools"
                    | "p1_core"
                    | "p2_resources"
                    | "p3_media"
                    | "p4_extended"
                    | "p5_management"
            )
        {
            return Err(format!("invalid scope/priority for {}", family.id));
        }
        for path in &family.official_paths {
            if !path.starts_with(&surface.mount) {
                return Err(format!("out-of-surface family path {}: {path}", family.id));
            }
            if family.scope == "management_api" && !is_reserved_management_path(path) {
                return Err(format!(
                    "management family path is not fail-closed {}: {path}",
                    family.id
                ));
            }
        }
        if family
            .contexts
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            != allowed_contexts
        {
            return Err(format!("incomplete family contexts {}", family.id));
        }
        for (context, status) in &family.contexts {
            if !matches!(
                status.coverage.as_str(),
                "complete" | "partial" | "transport_only" | "none" | "reserved"
            ) || !matches!(status.conversion.as_str(), "c0" | "c1" | "c2" | "c3")
                || !matches!(
                    status.maturity.as_str(),
                    "l0" | "l1" | "l2" | "l3" | "l4" | "l5" | "l6"
                )
                || status.evidence.trim().is_empty()
            {
                return Err(format!(
                    "invalid family context {} for {context}",
                    family.id
                ));
            }
            if family.scope == "management_api"
                && (status.coverage != "reserved" || status.conversion != "c3")
            {
                return Err(format!(
                    "management family is not reserved/C3 {}",
                    family.id
                ));
            }
        }
        for operation_id in &family.typed_operation_ids {
            if operation_surfaces.get(operation_id.as_str()).copied()
                != Some(family.surface.as_str())
            {
                return Err(format!(
                    "family {} references invalid operation {operation_id}",
                    family.id
                ));
            }
            if let Some(previous) =
                assigned_operations.insert(operation_id.as_str(), family.id.as_str())
            {
                return Err(format!(
                    "operation {operation_id} belongs to both {previous} and {}",
                    family.id
                ));
            }
        }
    }
    if family_surfaces != surfaces.keys().copied().collect::<BTreeSet<_>>() {
        return Err(format!(
            "official family inventory misses surfaces: {family_surfaces:?}"
        ));
    }
    for operation_id in operation_surfaces.keys() {
        if !assigned_operations.contains_key(operation_id) {
            return Err(format!(
                "typed operation {operation_id} is absent from official family inventory"
            ));
        }
    }

    for operation in &contract.operations {
        let surface = surfaces
            .get(operation.surface.as_str())
            .ok_or_else(|| format!("operation {} references unknown surface", operation.id))?;
        let expected_id = format!(
            "{}.{}",
            operation.surface,
            operation.id.split_once('.').map_or("", |(_, name)| name)
        );
        if operation.id != expected_id
            || !operation.path.starts_with(&surface.mount)
            || operation.response_modes.is_empty()
        {
            return Err(format!("invalid operation identity/path {}", operation.id));
        }
        if let Some(protocol) = operation.protocol.as_deref() {
            if !surface.protocols.iter().any(|item| item == protocol) {
                return Err(format!(
                    "operation {} references undeclared protocol {protocol}",
                    operation.id
                ));
            }
        }
        if !matches!(operation.transport.as_str(), "http" | "websocket")
            || !matches!(
                operation.request_body_mode.as_str(),
                "none" | "small_buffered" | "streaming_upload" | "duplex_frames"
            )
            || operation.response_modes.iter().any(|mode| {
                !matches!(
                    mode.as_str(),
                    "buffered" | "sse" | "binary_stream" | "websocket"
                )
            })
        {
            return Err(format!("invalid transport/body mode {}", operation.id));
        }
        let required_contexts = operation
            .required_context_declarations
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let required_evidence = operation
            .required_conversion_evidence
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        if !required_contexts.is_subset(&allowed_contexts)
            || !required_evidence.is_subset(&allowed_evidence)
        {
            return Err(format!("unknown context/evidence value {}", operation.id));
        }
        match operation.execution_policy.as_str() {
            "aggregated_control"
                if operation.ownership == "aggregated_control"
                    && required_contexts.is_empty()
                    && operation.request_body_mode == "none" => {}
            "capability_routed"
                if operation.ownership == "routed_known"
                    && required_contexts.contains("api_credential")
                    && required_contexts.contains("platform_supply")
                    && required_contexts.is_subset(&allowed_contexts)
                    && matches!(
                        operation.request_body_mode.as_str(),
                        "small_buffered" | "none" | "duplex_frames"
                    ) => {}
            "native_backend_only"
                if operation.ownership == "routed_known"
                    && required_contexts == BTreeSet::from(["api_credential"])
                    && matches!(
                        operation.request_body_mode.as_str(),
                        "small_buffered" | "streaming_upload" | "none" | "duplex_frames"
                    ) => {}
            "local_backend_only"
                if operation.ownership == "routed_known"
                    && required_contexts
                        == BTreeSet::from(["api_credential", "subscription_driver"])
                    && matches!(
                        operation.request_body_mode.as_str(),
                        "small_buffered" | "streaming_upload" | "none" | "duplex_frames"
                    ) => {}
            _ => {
                return Err(format!(
                    "execution policy does not match ownership for {}",
                    operation.id
                ));
            }
        }
        if operation
            .response_modes
            .iter()
            .any(|mode| matches!(mode.as_str(), "sse" | "binary_stream" | "websocket"))
            && (!required_evidence.contains("stream") || !required_evidence.contains("cancel"))
        {
            return Err(format!(
                "stream operation {} lacks stream/cancel evidence",
                operation.id
            ));
        }
    }
    Ok(())
}

#[test]
fn machine_contract_is_well_formed_and_matches_rust_registry() {
    let contract = parse_contract();
    validate_contract(&contract).expect("api surface contract semantics");
    contract_matches_registry(&contract).expect("api surface contract/Rust parity");
}

#[test]
fn surface_manifest_metadata_matches_machine_contract() {
    let contract = parse_contract();
    let manifest = surface_manifest();
    assert_eq!(manifest["version"], contract.manifest_version);
    assert_eq!(
        manifest["contract"]["schema_version"],
        contract.schema_version
    );
    assert_eq!(
        manifest["contract"]["execution_contexts"],
        serde_json::to_value(&contract.execution_contexts).unwrap()
    );
    assert_eq!(
        manifest["contract"]["conversion_levels"],
        serde_json::to_value(&contract.conversion_levels).unwrap()
    );
    assert_eq!(
        manifest["contract"]["maturity_levels"],
        serde_json::to_value(&contract.maturity_levels).unwrap()
    );
    assert_eq!(
        manifest["contract"]["support_evidence"],
        "declared_per_backend"
    );
    assert_eq!(
        manifest["official_families"],
        serde_json::to_value(&contract.official_families).unwrap()
    );

    let manifest_surfaces = manifest["surfaces"].as_array().expect("manifest surfaces");
    for surface in contract.surfaces {
        let actual = manifest_surfaces
            .iter()
            .find(|entry| entry["id"] == surface.id)
            .unwrap_or_else(|| panic!("manifest omitted surface {}", surface.id));
        assert_eq!(actual["mount"], surface.mount);
        assert_eq!(
            actual["protocols"],
            serde_json::to_value(surface.protocols).unwrap()
        );
        assert_eq!(
            actual["authentication"],
            serde_json::to_value(surface.authentication).unwrap()
        );
    }
}

#[test]
fn every_public_model_family_has_complete_native_api_transport() {
    let contract = parse_contract();
    for family in contract
        .official_families
        .iter()
        .filter(|family| family.scope == "model_api" && family.id != "openai.codex_live")
    {
        let native = &family.contexts["api_credential"];
        assert_eq!(
            native.coverage, "complete",
            "{} must remain transparently available to a matching native API channel",
            family.id
        );
        assert_eq!(native.conversion, "c0", "{} native conversion", family.id);
        assert!(
            matches!(native.maturity.as_str(), "l4" | "l5" | "l6"),
            "{} native API coverage lacks end-to-end evidence",
            family.id
        );
    }
}

#[test]
fn private_codex_live_does_not_claim_official_api_key_support() {
    let contract = parse_contract();
    let live = contract
        .official_families
        .iter()
        .find(|family| family.id == "openai.codex_live")
        .expect("Codex Live family");
    assert_eq!(live.contexts["api_credential"].coverage, "partial");
    assert_eq!(live.contexts["api_credential"].maturity, "l2");
    assert!(
        live.contexts["api_credential"]
            .evidence
            .contains("not_official_api_key")
    );
    assert_eq!(live.contexts["platform_supply"].coverage, "partial");
    assert_eq!(live.contexts["platform_supply"].maturity, "l2");
}

#[test]
fn parity_gate_rejects_contract_drift() {
    let mut contract = parse_contract();
    contract.operations[0].path.push_str("/mutated");
    assert!(
        contract_matches_registry(&contract).is_err(),
        "a contract-only path change must fail parity"
    );
}
