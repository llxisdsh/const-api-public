import { describe, expect, it } from "vitest";

import { channelFieldProfile, generateLocalApiKey, supplierChannelModelCount } from "./appHelpers";

describe("supplierChannelModelCount", () => {
  it("uses the complete catalog instead of the filtered running count", () => {
    const channel = { models: Array.from({ length: 17 }, (_, i) => `model-${i}`), upstream_model: "model-0", public_model: "model-0" };
    expect(supplierChannelModelCount(channel, { model_count: 5 }, { accepted_models: channel.models.slice(0, 5) })).toBe(17);
    expect(supplierChannelModelCount(channel, { model_count: 0 })).toBe(17);
  });
  it("keeps route and legacy count fallbacks consistent with the channel list", () => {
    const channel = { models: [], upstream_model: "", public_model: "" };
    expect(supplierChannelModelCount(channel)).toBe(0);
    expect(supplierChannelModelCount(channel, { model_count: 3 })).toBe(3);
    expect(supplierChannelModelCount(channel, undefined, { accepted_models: ["a"], unsupported_models: ["b", "c"] })).toBe(3);
    expect(supplierChannelModelCount({ ...channel, public_model: "legacy" })).toBe(1);
  });
});

describe("generateLocalApiKey", () => {
  it("creates distinct 128-bit local keys with the compact shared format", () => {
    const first = generateLocalApiKey();
    const second = generateLocalApiKey();

    expect(first).toMatch(/^sk-api-[0-9a-f]{32}$/);
    expect(first).toHaveLength(39);
    expect(second).not.toBe(first);
  });
});

describe("channelFieldProfile", () => {
  it("locks preset API channel Base URLs", () => {
    expect(channelFieldProfile("openai", "openai_api").baseUrlReadOnly).toBe(true);
    expect(channelFieldProfile("openai_compatible", "deepseek_api").baseUrlReadOnly).toBe(true);
    expect(channelFieldProfile("openrouter", "openrouter").baseUrlReadOnly).toBe(true);
    expect(channelFieldProfile("custom_endpoint", "omniroute").baseUrlReadOnly).toBe(true);
    for (const id of ["opencode_go", "opencode_zen", "kilo_gateway", "cline_api"] as const) {
      expect(channelFieldProfile("openai_compatible", id)).toMatchObject({
        showApiKey: true, showBaseUrl: true, baseUrlReadOnly: true,
      });
    }
  });

  it("keeps already-saved custom and local-runtime channels maintainable", () => {
    expect(channelFieldProfile("custom_endpoint", "custom_endpoint").baseUrlReadOnly).toBe(false);
    expect(channelFieldProfile("lan_share", "lan_share").baseUrlReadOnly).toBe(false);
    expect(channelFieldProfile("local_model", "ollama").baseUrlReadOnly).toBe(false);
    expect(channelFieldProfile("local_model", "lm_studio").baseUrlReadOnly).toBe(false);
    expect(channelFieldProfile("local_model", "vllm").baseUrlReadOnly).toBe(false);
  });

  it("does not expose HTTP fields for subscription adapters", () => {
    expect(channelFieldProfile("subscription_adapter", "openai_subscription")).toEqual(
      expect.objectContaining({
        showSubscriptionFields: true,
        showBaseUrl: false,
        showApiKey: false,
      }),
    );
  });
});
