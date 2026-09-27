import contractJson from "./api-surface-contract.json";

export type APIExecutionContext = "api_credential" | "subscription_driver" | "platform_supply";
export type APIConversionEvidence = "request" | "response" | "stream" | "error" | "cancel";
export type APIConversionLevel = "c0" | "c1" | "c2" | "c3";
export type APIMaturityLevel = "l0" | "l1" | "l2" | "l3" | "l4" | "l5" | "l6";
export type APIFamilyCoverage = "complete" | "partial" | "transport_only" | "none" | "reserved";

export interface APIFamilyContextStatus {
  readonly coverage: APIFamilyCoverage;
  readonly conversion: APIConversionLevel;
  readonly maturity: APIMaturityLevel;
  readonly evidence: string;
}

export interface APIOfficialFamilyContract {
  readonly id: string;
  readonly surface: "openai" | "anthropic" | "gemini";
  readonly name: string;
  readonly scope: "model_api" | "management_api";
  readonly priority:
    | "p0_tools"
    | "p1_core"
    | "p2_resources"
    | "p3_media"
    | "p4_extended"
    | "p5_management";
  readonly official_paths: readonly string[];
  readonly typed_operation_ids: readonly string[];
  readonly contexts: Readonly<Record<APIExecutionContext, APIFamilyContextStatus>>;
  readonly official_source: string;
  readonly verified_on: string;
}

export interface APISurfaceOperationContract {
  readonly id: string;
  readonly surface: "openai" | "anthropic" | "gemini";
  readonly method: string;
  readonly path: string;
  readonly protocol: "openai_responses" | "openai_chat" | "anthropic_messages" | "gemini_native" | null;
  readonly transport: "http" | "websocket";
  readonly request_body_mode: "none" | "small_buffered" | "streaming_upload" | "duplex_frames";
  readonly response_modes: readonly ("buffered" | "sse" | "binary_stream" | "websocket")[];
  readonly ownership: "aggregated_control" | "routed_known";
  readonly statefulness:
    | "stateless"
    | "resource_create"
    | "resource_read"
    | "resource_mutate"
    | "resource_delete";
  readonly billing_class: "none" | "metered" | "conditional";
  readonly execution_policy:
    | "aggregated_control"
    | "capability_routed"
    | "local_backend_only"
    | "native_backend_only";
  readonly required_context_declarations: readonly APIExecutionContext[];
  readonly required_conversion_evidence: readonly APIConversionEvidence[];
}

export interface APISurfaceContract {
  readonly schema_version: number;
  readonly manifest_version: number;
  readonly execution_contexts: readonly {
    readonly id: APIExecutionContext;
    readonly short: "A" | "S" | "P";
  }[];
  readonly conversion_levels: readonly ("c0" | "c1" | "c2" | "c3")[];
  readonly maturity_levels: readonly APIMaturityLevel[];
  readonly surfaces: readonly {
    readonly id: "openai" | "anthropic" | "gemini";
    readonly mount: string;
    readonly api_versions: readonly string[];
    readonly protocols: readonly string[];
    readonly authentication: readonly string[];
  }[];
  readonly official_families: readonly APIOfficialFamilyContract[];
  readonly operations: readonly APISurfaceOperationContract[];
}

export const API_SURFACE_CONTRACT = contractJson as unknown as APISurfaceContract;

export function operationsForSurface(
  surface: "openai" | "anthropic" | "gemini",
): readonly APISurfaceOperationContract[] {
  return API_SURFACE_CONTRACT.operations.filter((operation) => operation.surface === surface);
}

export function operationContractByID(id: string): APISurfaceOperationContract | undefined {
  return API_SURFACE_CONTRACT.operations.find((operation) => operation.id === id);
}

export function officialFamiliesForSurface(
  surface: "openai" | "anthropic" | "gemini",
): readonly APIOfficialFamilyContract[] {
  return API_SURFACE_CONTRACT.official_families.filter((family) => family.surface === surface);
}
