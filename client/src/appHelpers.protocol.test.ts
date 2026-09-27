import { describe, expect, it } from "vitest";

import {
  channelPrimaryProtocolOptions,
  protocolDebugOutcome,
  routePlanRequestBody,
  subscriptionSupportedProtocols,
} from "./appHelpers";
import type { ProtocolDebugResult } from "./appTypes";

function debugResult(overrides: Partial<ProtocolDebugResult> = {}): ProtocolDebugResult {
  return {
    target_type: "platform_auto",
    target_id: "",
    inbound_protocol: "anthropic_messages",
    target_protocol: "anthropic_messages",
    requested_model: "claude-sonnet-4-6",
    upstream_model: "claude-sonnet-4-6",
    conversion_level: "native",
    path: "/anthropic/v1/messages",
    request_headers: {},
    request_body: {},
    unsupported_fields: [],
    lossy_warnings: [],
    executed: true,
    http_status: 200,
    latency_ms: 10,
    content_type: "application/json",
    content: "ok",
    raw: "{}",
    metrics: {
      request_body_bytes: 2,
      response_raw_bytes: 2,
      response_content_bytes: 2,
      response_content_chars: 2,
      response_raw_lines: 1,
      response_sse_events: 0,
      response_sse_done: false,
      response_sse_last_event_type: "",
      response_kind: "text",
      tool_call_count: 0,
      finish_reason: "end_turn",
    },
    error_layer: "",
    ...overrides,
  };
}

describe("channelPrimaryProtocolOptions", () => {
  it("limits compatible presets to protocols declared by the source driver", () => {
    expect(channelPrimaryProtocolOptions("openai_compatible", ["openai_chat"])).toEqual([
      { value: "openai_chat", label: "OpenAI Chat Completions" },
    ]);
  });

  it("preserves the manifest order for multi-protocol presets", () => {
    expect(
      channelPrimaryProtocolOptions("openai_compatible", [
        "openai_responses",
        "openai_chat",
      ]),
    ).toEqual([
      { value: "openai_responses", label: "OpenAI Responses" },
      { value: "openai_chat", label: "OpenAI Chat Completions" },
    ]);
  });

  it("keeps the generic compatible fallback for custom endpoints", () => {
    expect(channelPrimaryProtocolOptions("openai_compatible")).toHaveLength(4);
  });

  it("projects Grok subscriptions onto their retained Responses executor", () => {
    expect(subscriptionSupportedProtocols("grok")).toEqual(["openai_responses"]);
  });

  it("adds Claude compaction only when the request-side experiment is enabled", () => {
    const normal = routePlanRequestBody(
      "anthropic_messages",
      "claude-sonnet-4-6",
      "hello",
      false,
    );
    const experimental = routePlanRequestBody(
      "anthropic_messages",
      "claude-sonnet-4-6",
      "hello",
      false,
      { claudeServerSideCompaction: true },
    );

    expect(normal).not.toHaveProperty("context_management");
    expect(experimental).toMatchObject({
      context_management: {
        edits: [{
          type: "compact_20260112",
          trigger: { type: "input_tokens", value: 50_000 },
        }],
      },
    });
    expect(experimental).not.toHaveProperty("pause_after_compaction");
  });

  it("does not leak the Claude experiment into another request protocol", () => {
    expect(routePlanRequestBody(
      "openai_responses",
      "gpt-5.6",
      "hello",
      false,
      { claudeServerSideCompaction: true },
    )).not.toHaveProperty("context_management");
  });

  it("classifies Claude compaction success, no-trigger, and HTTP failure explicitly", () => {
    const requestBody = {
      context_management: { edits: [{ type: "compact_20260112" }] },
    };
    expect(protocolDebugOutcome(debugResult({
      request_body: requestBody,
      raw: '{"content":[{"type":"compaction","content":"opaque"}]}',
    }))).toBe("compaction_triggered");
    expect(protocolDebugOutcome(debugResult({ request_body: requestBody })))
      .toBe("compaction_not_triggered");
    expect(protocolDebugOutcome(debugResult({
      request_body: requestBody,
      http_status: 400,
      error_layer: "upstream",
      metrics: { ...debugResult().metrics, response_kind: "error" },
    }))).toBe("request_failed");
    expect(protocolDebugOutcome(debugResult({
      request_body: requestBody,
      raw: 'event: error\ndata: {"type":"error","error":{"message":"not entitled"}}\n\n',
    }))).toBe("request_failed");
  });

  it("does not mistake a preview or ordinary successful request for compaction", () => {
    expect(protocolDebugOutcome(debugResult({ executed: false, http_status: undefined })))
      .toBe("preview");
    expect(protocolDebugOutcome(debugResult({
      executed: false,
      http_status: undefined,
      error_layer: "debug_command",
      metrics: { ...debugResult().metrics, response_kind: "error" },
    }))).toBe("request_failed");
    expect(protocolDebugOutcome(debugResult())).toBe("request_succeeded");
  });
});
