import { describe, expect, it } from "vitest";
import { applyChannelDetection, channelV5FromRenderer, rendererChannelFromV5 } from "./appHelpers";
import { sourceDriverById, sourceDriverInitialChannelV5, type SourceDriverId } from "./sourceDrivers";
import type { ChannelDetectionResult } from "./appTypes";

const result = (models: string[]): ChannelDetectionResult => ({
  models, surface_results: [], model_capability_evidence: [], detection_evidence: [],
  quota_status: "unknown", remaining_ratio: 0, quota_windows: [], checks: [], warnings: [],
});

function channel(driver: SourceDriverId = "openrouter") {
  return rendererChannelFromV5({
    ...sourceDriverInitialChannelV5(sourceDriverById(driver), {
      id: "refresh-channel", name: "Refresh", credentialRef: "fixture-key",
    }),
    models: ["vendor/first", "vendor/selected"], default_model: "vendor/selected",
  });
}

describe("information refresh projection", () => {
  it("round-trips channel-wide capacity for API channels without inventing a subscription", () => {
    const draft = channel("openrouter");
    draft.max_concurrency = 7;
    draft.quota_reserve_percent = 15;

    const wire = channelV5FromRenderer(draft);
    const reloaded = rendererChannelFromV5(JSON.parse(JSON.stringify(wire)));

    expect(wire.max_concurrency).toBe(7);
    expect(wire.quota_reserve_percent).toBe(15);
    expect(wire.subscription).toBeUndefined();
    expect(reloaded.max_concurrency).toBe(7);
    expect(reloaded.quota_reserve_percent).toBe(15);
    expect(reloaded.subscription.max_concurrency).toBe(7);
    expect(reloaded.subscription.quota_reserve_percent).toBe(15);
  });

  it("keeps the selected model and complete semantic evidence returned by the metadata-only backend", () => {
    const original = channel();
    const refreshed = result(["vendor/first", "vendor/selected", "vendor/new"]);
    refreshed.model_capability_evidence = [{
      protocol: "openai_responses", model_pattern: "vendor/selected", custom_tool: true,
      verification_state: "verified", verified_at_unix: 42,
    }];
    refreshed.detection_evidence = [{ name: "previous check", status: "unsupported" }];
    const merged = applyChannelDetection(original, refreshed);
    expect(merged.default_model).toBe("vendor/selected");
    expect(merged.model_capability_evidence).toEqual(refreshed.model_capability_evidence);
    expect(merged.detection_evidence).toEqual(refreshed.detection_evidence);
    expect(merged.surfaces).toEqual(original.surfaces);
  });

  it("accepts an authoritative empty OpenRouter account catalog without resurrecting a stale default", () => {
    const merged = applyChannelDetection(channel(), result([]));
    expect(merged.models).toEqual([]);
    expect(merged.default_model).toBe("");
    expect(merged.upstream_model).toBe("");
    expect(merged.public_model).toBe("");
  });

  it("preserves other sources' empty-catalog failure behavior", () => {
    expect(() => applyChannelDetection(channel("openai_api"), result([]))).toThrow();
  });
});
