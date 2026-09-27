import { describe, expect, test } from "vitest";

import {
  cancelSupplierEditor,
  channelDetectionInputChanged,
  channelEditorSettingsChanged,
  channelRuntimeNeedsActivation,
  diagnosticToastEventKey,
  finishSupplierDeletion,
  mergeEndpointDiscoveryConfig,
  reconcileDetectedDefaultModel,
  resolveDebugModelSelection,
  supplierDeleteConfirmation,
  supplierDeletedNotice,
  supplierLifecyclePresentation,
} from "./supplierEditorState";

describe("supplier editor state", () => {
  test("runtime hot updates exclude connection and enablement changes", () => {
    const saved = {
      source_driver: "custom_endpoint", credential_ref: "fixture-key", executor: { type: "http_surface" },
      default_target: { surface: "open_ai", protocol: "openai_chat" }, discovery: {}, surfaces: [],
      enabled: true, node_id: "node", server_ws_url: "wss://one.test/supplier/ws", server_quic_url: "quic://one.test/supplier",
    };
    expect(channelRuntimeNeedsActivation(saved, { ...saved, max_concurrency: 7 } as typeof saved)).toBe(false);
    for (const key of ["credential_ref", "source_driver", "node_id", "server_ws_url", "server_quic_url"] as const) {
      expect(channelRuntimeNeedsActivation(saved, { ...saved, [key]: "changed" })).toBe(true);
    }
    expect(channelRuntimeNeedsActivation({ ...saved, enabled: false }, saved)).toBe(true);
    expect(channelRuntimeNeedsActivation(undefined, saved)).toBe(true);
  });
  test("preserves the configured default model by case-insensitive identity", () => {
    expect(reconcileDetectedDefaultModel(
      "GPT-5.6-LUNA",
      ["gpt-5.6-sol", "gpt-5.6-luna"],
    )).toEqual({
      defaultModel: "gpt-5.6-luna",
      models: ["gpt-5.6-sol", "gpt-5.6-luna"],
    });
  });

  test("falls back to the first detected model when the configured model disappeared", () => {
    expect(reconcileDetectedDefaultModel(
      "retired-model",
      ["new-primary", "new-secondary"],
    ).defaultModel).toBe("new-primary");
  });

  test("keeps the model currently selected in the debug console", () => {
    expect(resolveDebugModelSelection(
      "gpt-5.6-terra",
      "gpt-5.5",
      ["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.5"],
    )).toBe("gpt-5.6-terra");
  });

  test("initializes an empty debug selection from the channel default", () => {
    expect(resolveDebugModelSelection(
      "",
      "GPT-5.5",
      ["gpt-5.6-sol", "gpt-5.5"],
    )).toBe("gpt-5.5");
  });

  test("detection evidence does not invalidate its own connection input", () => {
    const saved = {
      source_driver: "custom_endpoint",
      credential_ref: "secret",
      executor: { type: "http_surface" },
      default_target: { surface: "open_ai", protocol: "openai_responses" },
      discovery: { strategy: "openai", catalog: true, quota: false, metadata: true },
      surfaces: [{
        surface: "open_ai",
        base_url: "https://example.test/v1",
        endpoint_profile: "openai",
        auth_scheme: "bearer",
        verification: { state: "declared", checked_at_unix: 0, summary: "" },
        protocols: [{
          protocol: "openai_responses",
          preferred: true,
          verification: { state: "declared", checked_at_unix: 0, summary: "" },
        }],
        operation_overrides: [],
      }],
    };
    const detected = structuredClone(saved);
    detected.surfaces[0].verification = { state: "verified", checked_at_unix: 42, summary: "ok" };
    detected.surfaces[0].protocols[0].verification = { state: "verified", checked_at_unix: 43, summary: "ok" };

    expect(channelDetectionInputChanged(saved, detected)).toBe(false);
    detected.surfaces[0].base_url = "https://other.example.test/v1";
    expect(channelDetectionInputChanged(saved, detected)).toBe(true);
  });

  test("detection output does not make an otherwise unchanged editor dirty", () => {
    const saved = {
      id: "channel-1",
      name: "Channel",
      source_driver: "custom_endpoint",
      enabled: true,
      share_enabled: true,
      credential_ref: "secret",
      executor: { type: "http_surface" },
      default_target: { surface: "open_ai", protocol: "openai_responses" },
      discovery: { strategy: "openai", catalog: true, quota: false, metadata: true },
      default_model: "model-a",
      models: ["model-a"],
      price_ratio: 1,
      node_id: "node",
      server_ws_url: "wss://platform.test/supplier/ws",
      server_quic_url: "platform.test:443",
      surfaces: [],
      model_capability_evidence: [],
      detection_evidence: [],
    };
    const detected = {
      ...saved,
      models: ["model-a", "model-b"],
      model_capability_evidence: [{ model: "model-a" }],
      detection_evidence: [{ name: "catalog", status: "ok" }],
    };

    expect(channelEditorSettingsChanged(saved, detected)).toBe(false);
    expect(channelEditorSettingsChanged(saved, { ...detected, name: "Renamed" })).toBe(true);
    expect(channelEditorSettingsChanged(saved, { ...detected, max_concurrency: 7 })).toBe(true);
    expect(channelEditorSettingsChanged(saved, { ...detected, quota_reserve_percent: 15 })).toBe(true);
  });

  test("user-agent edits require saving, detection and runtime activation", () => {
    const saved = {
      id: "channel-ua", name: "Channel", source_driver: "custom_endpoint",
      enabled: true, share_enabled: true, credential_ref: "fixture-key",
      executor: { type: "http_surface" }, discovery: {}, surfaces: [],
      default_target: { surface: "open_ai", protocol: "openai_chat" },
      default_model: "model-a", price_ratio: 1, node_id: "node",
      server_ws_url: "wss://platform.test/supplier/ws", server_quic_url: "platform.test:443",
    };
    const changed = { ...saved, user_agent_profile: "opencode" };
    expect(channelEditorSettingsChanged(saved, changed)).toBe(true);
    expect(channelDetectionInputChanged(saved, changed)).toBe(true);
    expect(channelRuntimeNeedsActivation(saved, changed)).toBe(true);
    expect(channelEditorSettingsChanged(saved, { ...saved, user_agent_profile: "" })).toBe(false);
    expect(channelDetectionInputChanged(changed, { ...changed, user_agent_profile: " OpenCode " })).toBe(false);
    expect(channelDetectionInputChanged(changed, saved)).toBe(true);
  });

  test("renaming a channel does not replay an unchanged diagnostic toast", () => {
    const original = diagnosticToastEventKey({
      id: "supplier-model-catalog-channel-1",
      title: "Old name 部分模型未入池",
      message: "服务器已接纳 9 个模型，另有 11 个模型暂不支持。",
      createdAt: 0,
    });
    const renamed = diagnosticToastEventKey({
      id: "supplier-model-catalog-channel-1",
      title: "New name 部分模型未入池",
      message: "服务器已接纳 9 个模型，另有 11 个模型暂不支持。",
      createdAt: 0,
    });

    expect(renamed).toBe(original);
  });

  test("endpoint refresh preserves an unsaved channel draft", () => {
    const current = {
      platform_id: "old-platform",
      registry_version: 1,
      endpoints: [{ name: "old", base_url: "https://old.example", enabled: true }],
      supplier: {
        server_ws_url: "wss://old.example/supplier/ws",
        server_quic_url: "quic://old.example:443/supplier",
      },
      channels: [
        {
          id: "saved",
          name: "Saved channel",
          server_ws_url: "wss://old.example/supplier/ws",
          server_quic_url: "quic://old.example:443/supplier",
        },
        {
          id: "draft",
          name: "Custom Endpoint",
          server_ws_url: "wss://old.example/supplier/ws",
          server_quic_url: "quic://old.example:443/supplier",
        },
      ],
    };

    const next = mergeEndpointDiscoveryConfig(current, {
      platform_id: "new-platform",
      version: 2,
      endpoints: [{ name: "new", base_url: "https://new.example", enabled: true }],
      serverWsUrl: "wss://new.example/supplier/ws",
      serverQuicUrl: "quic://new.example:443/supplier",
    });

    expect(next.platform_id).toBe("new-platform");
    expect(next.registry_version).toBe(2);
    expect(next.channels.map((channel) => channel.id)).toEqual(["saved", "draft"]);
    expect(next.channels[1].name).toBe("Custom Endpoint");
    expect(next.channels[1].server_quic_url).toBe("quic://new.example:443/supplier");
  });

  test("cancelling a new draft closes the detail instead of selecting the first saved channel", () => {
    const savedConfig = {
      channels: [{ id: "saved" }],
    };

    expect(cancelSupplierEditor("draft", savedConfig)).toEqual({
      config: savedConfig,
      closeDetail: true,
      selectedChannelId: "saved",
    });
  });

  test("cancelling edits to a saved channel keeps its detail open", () => {
    const savedConfig = {
      channels: [{ id: "saved" }],
    };

    expect(cancelSupplierEditor("saved", savedConfig)).toEqual({
      config: savedConfig,
      closeDetail: false,
      selectedChannelId: "saved",
    });
  });

  test("always asks for confirmation before deleting a channel", () => {
    expect(supplierDeleteConfirmation("Office API")).toEqual({
      title: "删除“Office API”？",
      message: "将删除本机保存的渠道配置；如果渠道正在运行，会先停止。此操作无法撤销。",
      confirmText: "删除渠道",
      tone: "danger",
    });
  });

  test("interpolates the deleted channel name in the success notice", () => {
    expect(supplierDeletedNotice("Office API", false)).toBe("Office API 已删除。");
    expect(supplierDeletedNotice("Office API", true)).toBe("Office API 已停止并删除。");
  });

  test("closes channel details after deletion instead of opening the next channel", () => {
    expect(finishSupplierDeletion([{
      id: "next-channel",
      models: ["model-a"],
    }])).toEqual({
      closeDetail: true,
      selectedChannelId: "next-channel",
      models: ["model-a"],
    });
  });

  test("keeps local readiness separate from platform registration", () => {
    expect(supplierLifecyclePresentation({
      saved: true,
      enabledForSupply: true,
      busy: false,
      detectionReady: true,
      localStatus: "available",
      accountState: "signed_in",
      runtimeRunning: true,
      connectedTransport: "quic",
      activeTransport: "starting",
    })).toMatchObject({
      configLabel: "配置：已保存",
      localLabel: "检测：已通过",
      platformLabel: "供应：注册中",
      actionLabel: "检测并连接平台",
    });
  });

  test("shows that a locally ready channel is waiting for login", () => {
    expect(supplierLifecyclePresentation({
      saved: true,
      enabledForSupply: true,
      busy: false,
      detectionReady: true,
      localStatus: "available",
      accountState: "signed_out",
      runtimeRunning: true,
      connectedTransport: "quic",
      activeTransport: "starting",
    }).platformLabel).toBe("供应：等待登录");
  });

  test("shows acknowledged fallback separately from platform online", () => {
    expect(supplierLifecyclePresentation({
      saved: true,
      enabledForSupply: true,
      busy: false,
      detectionReady: true,
      localStatus: "available",
      accountState: "signed_in",
      runtimeRunning: true,
      connectedTransport: "quic",
      activeTransport: "quic",
      routeState: "fallback",
      routeLabel: "回退池",
    })).toMatchObject({
      localLabel: "检测：已通过",
      platformLabel: "供应：回退池",
      platformTone: "pending",
      actionLabel: "检测并刷新",
    });
  });

  test("shows unsaved configuration without implying runtime state", () => {
    expect(supplierLifecyclePresentation({
      saved: false,
      enabledForSupply: false,
      busy: false,
      accountState: "signed_out",
      runtimeRunning: false,
    })).toMatchObject({
      configLabel: "配置：未保存",
      localLabel: "检测：待验证",
      platformLabel: "供应：未开启",
      actionLabel: "检测并开启供应",
    });
  });
});
