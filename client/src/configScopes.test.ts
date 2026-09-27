import { describe, expect, it } from "vitest";

import type { ClientConfig } from "./appTypes";
import { overlayAccessConfig, overlaySupplierConfig } from "./configScopes";

function configFixture(overrides: Partial<ClientConfig> = {}): ClientConfig {
  return {
    listen: "127.0.0.1:38787",
    allow_lan_access: false,
    proxy_auto_start: true,
    supplier_auto_start: false,
    api_key: "saved-key",
    allow_model_equivalence: false,
    prefer_local_supply: true,
    allow_unverified_platform_routes: false,
    channels: [{ id: "saved-channel" }],
    supplier: { name: "saved-supplier" },
    ...overrides,
  } as ClientConfig;
}

function channelFixture(
  overrides: Record<string, unknown>,
): ClientConfig["channels"][number] {
  // These scope tests exercise only identity and changed-field merging; the
  // full channel shape is covered by the source-driver normalization tests.
  return overrides as unknown as ClientConfig["channels"][number];
}

describe("configuration page scopes", () => {
  it("preserves an access draft without restoring stale supplier data", () => {
    const authoritative = configFixture({
      channels: [{ id: "fresh-channel" }] as ClientConfig["channels"],
    });
    const draft = configFixture({
      allow_model_equivalence: true,
      allow_unverified_platform_routes: true,
      api_key: "draft-key",
      channels: [{ id: "stale-channel" }] as ClientConfig["channels"],
    });

    const merged = overlayAccessConfig(authoritative, draft, configFixture());

    expect(merged.allow_model_equivalence).toBe(true);
    // The retired setting cannot be changed by an access-page draft.
    expect(merged.allow_unverified_platform_routes).toBe(authoritative.allow_unverified_platform_routes);
    expect(merged.api_key).toBe("draft-key");
    expect(merged.channels[0]?.id).toBe("fresh-channel");
  });

  it("preserves a supplier draft without restoring stale access settings", () => {
    const authoritative = configFixture({
      allow_model_equivalence: true,
      api_key: "fresh-key",
    });
    const draft = configFixture({
      allow_model_equivalence: false,
      api_key: "stale-key",
      channels: [{ id: "draft-channel" }] as ClientConfig["channels"],
    });

    const merged = overlaySupplierConfig(authoritative, draft, configFixture());

    expect(merged.allow_model_equivalence).toBe(true);
    expect(merged.api_key).toBe("fresh-key");
    expect(merged.channels[0]?.id).toBe("draft-channel");
  });

  it("keeps background changes for supplier channels the user did not edit", () => {
    const baseline = configFixture({
      channels: [
        channelFixture({ id: "edited", name: "old" }),
        channelFixture({ id: "refreshed", models: ["old-model"] }),
      ],
    });
    const authoritative = configFixture({
      channels: [
        channelFixture({ id: "edited", name: "old" }),
        channelFixture({ id: "refreshed", models: ["fresh-model"] }),
      ],
    });
    const draft = configFixture({
      channels: [
        channelFixture({ id: "edited", name: "new" }),
        channelFixture({ id: "refreshed", models: ["old-model"] }),
      ],
    });

    const merged = overlaySupplierConfig(authoritative, draft, baseline);

    expect(merged.channels.find((channel) => channel.id === "edited")?.name).toBe("new");
    expect(merged.channels.find((channel) => channel.id === "refreshed")?.models).toEqual(["fresh-model"]);
  });
});
