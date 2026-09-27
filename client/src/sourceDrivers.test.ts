import { describe, expect, test } from "vitest";
import sourceDriverManifestJson from "../source-drivers.json";
import {
  applyChannelDetectionV2,
  normalizeChannelV5,
  parseSourceDriverManifest,
  reloadChannelV5,
  serializeChannelV5,
  serializeClientConfigV5,
  SOURCE_DRIVER_MANIFEST_VERSION,
  SOURCE_DRIVERS,
  sourceDriverById,
  sourceDriversForApiAccess,
  sourceDriversForLanSharing,
  sourceDriversForCustom,
  sourceDriverInitialChannelDefaults,
  sourceDriverInitialChannelV5,
  validateChannelV5,
} from "./sourceDrivers";

describe("source drivers", () => {
  test("loads the shared manifest with readable localized metadata", () => {
    expect(SOURCE_DRIVER_MANIFEST_VERSION).toBe(2);
    expect(sourceDriverById("openai_subscription")).toEqual(expect.objectContaining({
      title: "OpenAI 账号订阅",
      description: "连接 ChatGPT 账号，使用账号订阅额度",
    }));
  });

  test("links each branded source to its official product or documentation", () => {
    expect(Object.fromEntries(SOURCE_DRIVERS.map((driver) => [driver.id, driver.homepageUrl]))).toEqual({
      openai_subscription: "https://chatgpt.com/",
      claude_subscription: "https://claude.ai/",
      gemini_subscription: "https://antigravity.google/",
      grok_subscription: "https://grok.com/",
      openai_api: "https://developers.openai.com/",
      anthropic_api: "https://docs.anthropic.com/",
      gemini_api: "https://ai.google.dev/",
      xai_api: "https://docs.x.ai/",
      mistral_api: "https://docs.mistral.ai/",
      deepseek_api: "https://api-docs.deepseek.com/",
      dashscope_api: "https://help.aliyun.com/zh/model-studio/",
      moonshot_api: "https://platform.moonshot.cn/docs/",
      zhipu_api: "https://docs.bigmodel.cn/",
      minimax_api: "https://platform.minimaxi.com/docs/",
      stepfun_api: "https://platform.stepfun.com/docs/",
      azure_openai: "https://ai.azure.com/",
      bedrock_mantle: "https://docs.aws.amazon.com/bedrock/latest/userguide/quotas-mantle.html",
      groq_api: "https://console.groq.com/docs/",
      together_api: "https://docs.together.ai/",
      fireworks_api: "https://docs.fireworks.ai/",
      perplexity_api: "https://docs.perplexity.ai/",
      huggingface_api: "https://huggingface.co/docs/inference-providers/",
      nvidia_api: "https://build.nvidia.com/",
      siliconflow_api: "https://docs.siliconflow.cn/",
      volcengine_ark_api: "https://www.volcengine.com/docs/82379/",
      baidu_qianfan_api: "https://cloud.baidu.com/doc/qianfan-api/",
      tencent_hunyuan_api: "https://cloud.tencent.com/document/product/1729/",
      openrouter: "https://openrouter.ai/docs/quickstart",
      opencode_go: "https://opencode.ai/docs/go/",
      opencode_zen: "https://opencode.ai/docs/zen/",
      kilo_gateway: "https://kilo.ai/docs/gateway",
      cline_api: "https://docs.cline.bot/api/overview",
      command_code: "https://commandcode.ai/docs/provider",
      kimi_code: "https://www.kimi.com/code/docs/en/",
      glm_coding_plan: "https://docs.z.ai/devpack/overview",
      minimax_token_plan: "https://platform.minimax.io/docs/token-plan/intro",
      ollama_cloud: "https://docs.ollama.com/cloud",
      omniroute: "https://github.com/diegosouzapw/OmniRoute",
      ollama: "https://ollama.com/",
      lm_studio: "https://lmstudio.ai/",
      vllm: "https://docs.vllm.ai/",
      lan_share: undefined,
      custom_endpoint: undefined,
    });
  });

  test("keeps regular API sources separate from LAN sharing in Add Channel", () => {
    const drivers = sourceDriversForApiAccess();
    expect(drivers[0]).toEqual(expect.objectContaining({
      id: "openai_api",
      title: "OpenAI 官方 API",
    }));
    expect(drivers.map((driver) => driver.id)).toEqual([
      "openai_api",
      "anthropic_api",
      "gemini_api",
      "xai_api",
      "mistral_api",
      "deepseek_api",
      "dashscope_api",
      "moonshot_api",
      "zhipu_api",
      "minimax_api",
      "stepfun_api",
      "azure_openai",
      "bedrock_mantle",
      "groq_api",
      "together_api",
      "fireworks_api",
      "perplexity_api",
      "huggingface_api",
      "nvidia_api",
      "siliconflow_api",
      "volcengine_ark_api",
      "baidu_qianfan_api",
      "tencent_hunyuan_api",
      "openrouter",
      "opencode_go",
      "opencode_zen",
      "kilo_gateway",
      "cline_api",
      "command_code",
      "kimi_code",
      "glm_coding_plan",
      "minimax_token_plan",
      "ollama_cloud",
    ]);
    expect(sourceDriversForLanSharing().map((driver) => driver.id)).toEqual(["lan_share"]);
    expect(sourceDriversForCustom().map((driver) => driver.id)).toEqual(["custom_endpoint"]);
  });

  test("rejects duplicate protocols inside one surface binding", () => {
    const invalid = structuredClone(sourceDriverManifestJson);
    const driver = invalid.drivers.find((candidate) => candidate.id === "openai_api");
    if (!driver) throw new Error("missing OpenAI driver fixture");
    driver.surfaces[0].protocols.push(driver.surfaces[0].protocols[0]);

    expect(() => parseSourceDriverManifest(invalid)).toThrow(/surface binding/i);
  });

  test("rejects an executor locator that conflicts with execution kind", () => {
    const invalid = structuredClone(sourceDriverManifestJson);
    const driver = invalid.drivers.find((candidate) => candidate.id === "openai_subscription");
    if (!driver) throw new Error("missing OpenAI subscription fixture");
    driver.executor = { type: "http_surface" };

    expect(() => parseSourceDriverManifest(invalid)).toThrow(/executor/i);
  });

  test("projects subscription operation evidence without treating approximate support as routable", () => {
    const openai = sourceDriverById("openai_subscription").subscriptionContract;
    const antigravity = sourceDriverById("gemini_subscription").subscriptionContract;

    expect(openai).toEqual(expect.objectContaining({
      version: 1,
      acceptedIngressProtocols: [
        "openai_responses",
        "openai_chat",
        "anthropic_messages",
        "gemini_native",
      ],
      generation: expect.objectContaining({
        modes: ["buffered", "sse"],
        defaultEnabled: true,
      }),
    }));
    expect(antigravity?.specialOperations).toEqual(expect.arrayContaining([
      expect.objectContaining({
        surface: "gemini",
        operation: "embed_content",
        capability: expect.objectContaining({
          conversion: expect.objectContaining({ request: "c2", response: "c2" }),
          defaultEnabled: false,
        }),
      }),
    ]));
  });

  test("requires retained subscriptions to declare a safe operation contract", () => {
    const missing = structuredClone(sourceDriverManifestJson);
    const retained = missing.drivers.find((candidate) => candidate.id === "openai_subscription");
    if (!retained) throw new Error("missing OpenAI subscription fixture");
    delete (retained as { subscription_contract?: unknown }).subscription_contract;
    expect(() => parseSourceDriverManifest(missing)).toThrow(/subscription contract/i);

    const approximate = structuredClone(sourceDriverManifestJson);
    const antigravity = approximate.drivers.find((candidate) => candidate.id === "gemini_subscription");
    if (!antigravity?.subscription_contract) throw new Error("missing Antigravity subscription contract");
    const embed = antigravity.subscription_contract.special_operations.find(
      (operation) => operation.operation === "embed_content",
    );
    if (!embed) throw new Error("missing Antigravity embed fixture");
    embed.capability.default_enabled = true;
    expect(() => parseSourceDriverManifest(approximate)).toThrow(/c2\/c3/i);
  });

  test("forbids HTTP sources from declaring subscription capabilities", () => {
    const invalid = structuredClone(sourceDriverManifestJson);
    const retained = invalid.drivers.find((candidate) => candidate.id === "openai_subscription");
    const direct = invalid.drivers.find((candidate) => candidate.id === "openai_api");
    if (!retained?.subscription_contract || !direct) throw new Error("missing source driver fixtures");
    (direct as typeof direct & { subscription_contract?: unknown }).subscription_contract =
      structuredClone(retained.subscription_contract);

    expect(() => parseSourceDriverManifest(invalid)).toThrow(/HTTP executor/i);
  });

  test("rejects incomplete manifests and invalid creation policy enums", () => {
    const incomplete = structuredClone(sourceDriverManifestJson);
    incomplete.drivers.pop();
    expect(() => parseSourceDriverManifest(incomplete)).toThrow(/every source driver/i);

    const invalidPolicy = structuredClone(sourceDriverManifestJson);
    invalidPolicy.drivers[0].creation.basic_verification = "catalog_only";
    expect(() => parseSourceDriverManifest(invalidPolicy)).toThrow(/creation policy/i);
  });

  test("lists every wizard source in stable category and order", () => {
    expect(SOURCE_DRIVERS.map((driver) => ({
      id: driver.id,
      category: driver.category,
      order: driver.order,
      iconKey: driver.iconKey,
    }))).toMatchInlineSnapshot(`
      [
        {
          "category": "subscription",
          "iconKey": "openai",
          "id": "openai_subscription",
          "order": 10,
        },
        {
          "category": "subscription",
          "iconKey": "claude",
          "id": "claude_subscription",
          "order": 20,
        },
        {
          "category": "subscription",
          "iconKey": "gemini",
          "id": "gemini_subscription",
          "order": 30,
        },
        {
          "category": "subscription",
          "iconKey": "xai",
          "id": "grok_subscription",
          "order": 40,
        },
        {
          "category": "official_api",
          "iconKey": "openai",
          "id": "openai_api",
          "order": 10,
        },
        {
          "category": "official_api",
          "iconKey": "anthropic",
          "id": "anthropic_api",
          "order": 20,
        },
        {
          "category": "official_api",
          "iconKey": "gemini",
          "id": "gemini_api",
          "order": 30,
        },
        {
          "category": "official_api",
          "iconKey": "xai",
          "id": "xai_api",
          "order": 40,
        },
        {
          "category": "official_api",
          "iconKey": "mistral",
          "id": "mistral_api",
          "order": 50,
        },
        {
          "category": "official_api",
          "iconKey": "deepseek",
          "id": "deepseek_api",
          "order": 60,
        },
        {
          "category": "official_api",
          "iconKey": "dashscope",
          "id": "dashscope_api",
          "order": 70,
        },
        {
          "category": "official_api",
          "iconKey": "moonshot",
          "id": "moonshot_api",
          "order": 80,
        },
        {
          "category": "official_api",
          "iconKey": "zhipu",
          "id": "zhipu_api",
          "order": 90,
        },
        {
          "category": "official_api",
          "iconKey": "minimax",
          "id": "minimax_api",
          "order": 100,
        },
        {
          "category": "official_api",
          "iconKey": "stepfun",
          "id": "stepfun_api",
          "order": 110,
        },
        {
          "category": "cloud_platform",
          "iconKey": "azure",
          "id": "azure_openai",
          "order": 10,
        },
        {
          "category": "cloud_platform",
          "iconKey": "bedrock",
          "id": "bedrock_mantle",
          "order": 20,
        },
        {
          "category": "cloud_platform",
          "iconKey": "groq",
          "id": "groq_api",
          "order": 30,
        },
        {
          "category": "cloud_platform",
          "iconKey": "together",
          "id": "together_api",
          "order": 40,
        },
        {
          "category": "cloud_platform",
          "iconKey": "fireworks",
          "id": "fireworks_api",
          "order": 50,
        },
        {
          "category": "cloud_platform",
          "iconKey": "perplexity",
          "id": "perplexity_api",
          "order": 60,
        },
        {
          "category": "cloud_platform",
          "iconKey": "huggingface",
          "id": "huggingface_api",
          "order": 70,
        },
        {
          "category": "cloud_platform",
          "iconKey": "nvidia",
          "id": "nvidia_api",
          "order": 80,
        },
        {
          "category": "cloud_platform",
          "iconKey": "siliconflow",
          "id": "siliconflow_api",
          "order": 90,
        },
        {
          "category": "cloud_platform",
          "iconKey": "volcengine",
          "id": "volcengine_ark_api",
          "order": 100,
        },
        {
          "category": "cloud_platform",
          "iconKey": "baidu",
          "id": "baidu_qianfan_api",
          "order": 110,
        },
        {
          "category": "cloud_platform",
          "iconKey": "tencent",
          "id": "tencent_hunyuan_api",
          "order": 120,
        },
        {
          "category": "local_runtime",
          "iconKey": "ollama",
          "id": "ollama",
          "order": 10,
        },
        {
          "category": "local_runtime",
          "iconKey": "lmstudio",
          "id": "lm_studio",
          "order": 20,
        },
        {
          "category": "local_runtime",
          "iconKey": "vllm",
          "id": "vllm",
          "order": 30,
        },
        {
          "category": "compatible_gateway",
          "iconKey": "openrouter",
          "id": "openrouter",
          "order": 10,
        },
        {
          "category": "compatible_gateway",
          "iconKey": "opencode",
          "id": "opencode_go",
          "order": 11,
        },
        {
          "category": "compatible_gateway",
          "iconKey": "opencode",
          "id": "opencode_zen",
          "order": 12,
        },
        {
          "category": "compatible_gateway",
          "iconKey": "custom",
          "id": "kilo_gateway",
          "order": 13,
        },
        {
          "category": "compatible_gateway",
          "iconKey": "cline",
          "id": "cline_api",
          "order": 14,
        },
        {
          "category": "compatible_gateway",
          "iconKey": "custom",
          "id": "command_code",
          "order": 15,
        },
        {
          "category": "compatible_gateway",
          "iconKey": "moonshot",
          "id": "kimi_code",
          "order": 16,
        },
        {
          "category": "compatible_gateway",
          "iconKey": "zhipu",
          "id": "glm_coding_plan",
          "order": 17,
        },
        {
          "category": "compatible_gateway",
          "iconKey": "minimax",
          "id": "minimax_token_plan",
          "order": 18,
        },
        {
          "category": "compatible_gateway",
          "iconKey": "ollama",
          "id": "ollama_cloud",
          "order": 19,
        },
        {
          "category": "compatible_gateway",
          "iconKey": "omniroute",
          "id": "omniroute",
          "order": 20,
        },
        {
          "category": "compatible_gateway",
          "iconKey": "network",
          "id": "lan_share",
          "order": 30,
        },
        {
          "category": "compatible_gateway",
          "iconKey": "custom",
          "id": "custom_endpoint",
          "order": 90,
        },
      ]
    `);
  });

  test("encodes the complete V2 defaults used to create every channel", () => {
    expect(SOURCE_DRIVERS.map(sourceDriverInitialChannelDefaults)).toMatchSnapshot();
  });

  test("keeps subscription drivers to their known native contracts", () => {
    const subscriptions = SOURCE_DRIVERS.filter((driver) => driver.category === "subscription");

    expect(subscriptions.map((driver) => [driver.id, driver.supportedProtocols])).toEqual([
      ["openai_subscription", ["openai_responses"]],
      ["claude_subscription", ["anthropic_messages"]],
      ["gemini_subscription", ["gemini_native"]],
      ["grok_subscription", ["openai_responses"]],
    ]);
    expect(subscriptions.map((driver) => driver.executor)).toEqual([
      { type: "retained_subscription", provider: "openai" },
      { type: "retained_subscription", provider: "claude" },
      { type: "retained_subscription", provider: "antigravity" },
      { type: "retained_subscription", provider: "grok" },
    ]);
    expect(subscriptions.flatMap((driver) => driver.surfaceBindings.map((binding) => binding.baseUrl)))
      .toEqual([undefined, undefined, undefined, undefined]);
  });

  test("preserves the official multi-surface defaults", () => {
    expect(sourceDriverById("openai_api").surfaceBindings).toEqual([
      expect.objectContaining({ surface: "open_ai", protocols: ["openai_responses", "openai_chat"], preferredProtocol: "openai_responses" }),
    ]);
    expect(sourceDriverById("gemini_api").surfaceBindings).toEqual([
      expect.objectContaining({ surface: "gemini", baseUrl: "https://generativelanguage.googleapis.com", protocols: ["gemini_native"], preferredProtocol: "gemini_native" }),
      expect.objectContaining({ surface: "open_ai", baseUrl: "https://generativelanguage.googleapis.com/v1beta/openai", protocols: ["openai_chat"], preferredProtocol: "openai_chat" }),
    ]);
    expect(sourceDriverById("bedrock_mantle").surfaceBindings.map((binding) => binding.surface)).toEqual(["open_ai", "anthropic"]);
  });

  test("keeps researched provider endpoints and preferred protocols explicit", () => {
    expect(sourceDriverById("xai_api")).toEqual(expect.objectContaining({
      defaultTarget: { surface: "open_ai", protocol: "openai_responses" },
      baseUrl: "https://api.x.ai/v1",
    }));
    expect(sourceDriverById("deepseek_api").surfaceBindings).toEqual([
      expect.objectContaining({ surface: "open_ai", baseUrl: "https://api.deepseek.com", protocols: ["openai_chat", "openai_responses"], preferredProtocol: "openai_chat" }),
      expect.objectContaining({ surface: "anthropic", baseUrl: "https://api.deepseek.com/anthropic", protocols: ["anthropic_messages"] }),
    ]);
    expect(sourceDriverById("dashscope_api").surfaceBindings).toEqual([
      expect.objectContaining({ baseUrl: "https://dashscope.aliyuncs.com/compatible-mode/v1", protocols: ["openai_chat", "openai_responses"] }),
    ]);
    expect(sourceDriverById("minimax_api").surfaceBindings).toEqual([
      expect.objectContaining({ surface: "open_ai", baseUrl: "https://api.minimaxi.com/v1", protocols: ["openai_chat"] }),
      expect.objectContaining({ surface: "anthropic", baseUrl: "https://api.minimaxi.com/anthropic", protocols: ["anthropic_messages"] }),
    ]);
    expect(sourceDriverById("fireworks_api").surfaceBindings).toEqual([
      expect.objectContaining({ surface: "open_ai", baseUrl: "https://api.fireworks.ai/inference/v1", protocols: ["openai_responses", "openai_chat"] }),
      expect.objectContaining({ surface: "anthropic", baseUrl: "https://api.fireworks.ai/inference", protocols: ["anthropic_messages"] }),
    ]);
    expect(sourceDriverById("huggingface_api").supportedProtocols).toEqual(["openai_chat", "openai_responses"]);
    expect(sourceDriverById("zhipu_api").baseUrl).toBe("https://open.bigmodel.cn/api/paas/v4");
    expect(sourceDriverById("volcengine_ark_api").baseUrl).toBe("https://ark.cn-beijing.volces.com/api/v3");
    expect(sourceDriverById("volcengine_ark_api").supportedProtocols).toEqual(["openai_chat", "openai_responses"]);
    expect(sourceDriverById("baidu_qianfan_api").surfaceBindings.map((binding) => binding.baseUrl)).toEqual([
      "https://qianfan.baidubce.com/v2",
      "https://qianfan.baidubce.com/anthropic",
    ]);
    expect(sourceDriverById("openrouter").surfaceBindings).toEqual([
      expect.objectContaining({ surface: "open_ai", baseUrl: "https://openrouter.ai/api/v1", protocols: ["openai_chat", "openai_responses"] }),
      expect.objectContaining({ surface: "anthropic", baseUrl: "https://openrouter.ai/api", protocols: ["anthropic_messages"] }),
    ]);
  });

  test("keeps OmniRoute a branded shortcut over the generic custom endpoint executor", () => {
    const driver = sourceDriverById("omniroute");

    expect(driver).toEqual(expect.objectContaining({
      kind: "custom_endpoint",
      executionKind: "http_surface",
      homepageUrl: "https://github.com/diegosouzapw/OmniRoute",
      defaultTarget: { surface: "open_ai", protocol: "openai_chat" },
    }));
    expect(driver.surfaceBindings).toEqual([
      expect.objectContaining({
        baseUrl: "http://127.0.0.1:20128/v1",
        endpointProfile: "openai_compatible",
        authScheme: "bearer",
        protocols: ["openai_chat"],
      }),
    ]);
  });

  test("never schedules conversational probes for periodic detection", () => {
    for (const driver of SOURCE_DRIVERS) {
      expect(driver.detection.periodic.conversationalProbe).toBe(false);
    }
  });

  test("builds fresh channel defaults without exposing mutable source identity", () => {
    const driver = sourceDriverById("gemini_api");
    const defaults = sourceDriverInitialChannelDefaults(driver);
    expect(defaults.sourceDriver).toBe("gemini_api");
    defaults.surfaceBindings[0].protocols[0].preferred = false;

    expect(sourceDriverById("gemini_api").surfaceBindings[0].preferredProtocol).toBe("gemini_native");
  });

  test.each(["opencode_go", "opencode_zen", "kilo_gateway", "cline_api", "command_code", "kimi_code", "glm_coding_plan", "minimax_token_plan", "ollama_cloud"] as const)(
    "%s fixes its connection while retaining editable credentials and supply settings",
    (id) => {
      const driver = sourceDriverById(id);
      expect(driver.fixedConnection).toBe(true);
      expect(driver.kind).toBe("openai_compatible");
      const channel = sourceDriverInitialChannelV5(driver, { id, name: driver.title });
      channel.credential_ref = "new-key";
      channel.max_concurrency = 7;
      channel.price_ratio = 0.5;
      channel.surfaces[0].base_url += "/";
      channel.surfaces[0].verification.state = "verified";
      expect(reloadChannelV5(serializeChannelV5(channel)).credential_ref).toBe("new-key");

      for (const mutate of [
        (value: typeof channel) => { value.surfaces[0].base_url = "https://proxy.example.test/v1"; },
        (value: typeof channel) => { value.surfaces[0].base_url += "other-plan"; },
        (value: typeof channel) => { value.surfaces[0].auth_scheme = "x_api_key"; },
        (value: typeof channel) => { value.surfaces[0].endpoint_profile = "custom"; },
        (value: typeof channel) => { value.discovery = { ...value.discovery, strategy: "custom" }; },
        (value: typeof channel) => {
          value.default_target = { ...value.default_target, protocol: "openai_responses" };
          value.surfaces[0].protocols = [{ protocol: "openai_responses", preferred: true,
            verification: { state: "declared", checked_at_unix: 0, summary: "" } }];
        },
      ]) {
        const changed = structuredClone(channel);
        mutate(changed);
        expect(() => serializeChannelV5(changed)).toThrow("fixed official endpoints and protocols");
        expect(() => reloadChannelV5(changed)).toThrow("fixed official endpoints and protocols");
      }
    },
  );

  test("upgrades the old fixed Zen preset without accepting altered connections", () => {
    const old = sourceDriverInitialChannelV5(sourceDriverById("opencode_zen"), {
      id: "legacy-zen", name: "My Zen", credentialRef: "kept-key",
    });
    old.surfaces = old.surfaces.filter((surface) => surface.surface !== "gemini");
    old.models = ["gemini-3.8-flash", "gpt-5.6-luna"];
    old.max_concurrency = 7;
    old.surfaces[0].verification.state = "verified";
    const upgraded = reloadChannelV5(old);
    expect(upgraded.credential_ref).toBe("kept-key");
    expect(upgraded.models).toEqual(old.models);
    expect(upgraded.max_concurrency).toBe(7);
    expect(upgraded.surfaces.slice(0, 2)).toEqual(old.surfaces);
    expect(upgraded.surfaces[2]).toMatchObject({
      surface: "gemini", base_url: "https://opencode.ai/zen/v1",
      auth_scheme: "x_goog_api_key", verification: { state: "declared" },
    });
    expect(reloadChannelV5(serializeChannelV5(upgraded))).toEqual(upgraded);
    old.surfaces[0].base_url = "https://proxy.example.test/v1";
    expect(() => reloadChannelV5(old)).toThrow("fixed official endpoints and protocols");
    expect(sourceDriverById("opencode_go").supportedProtocols).not.toContain("gemini_native");
  });

  test("keeps manifest execution, target, discovery, protocols, and surfaces as V2 truth", () => {
    for (const driver of SOURCE_DRIVERS) {
      const defaults = sourceDriverInitialChannelDefaults(driver);
      expect(defaults.executionKind).toBe(driver.executionKind);
      expect(defaults.executor).toEqual(driver.executor);
      expect(defaults.defaultTarget).toEqual(driver.defaultTarget);
      expect(defaults.discovery).toEqual(driver.discovery);
      expect(defaults.surfaces).toEqual(defaults.surfaceBindings);
      expect(defaults.surfaces.some((binding) =>
        binding.surface === defaults.defaultTarget.surface
          && binding.protocols.some((protocol) => protocol.protocol === defaults.defaultTarget.protocol),
      )).toBe(true);
      for (const binding of defaults.surfaces) {
        expect(binding.operation_overrides).toEqual([]);
        expect(binding.verification.state).toBe("declared");
        expect(binding.protocols.every((protocol) => protocol.verification.state === "declared")).toBe(true);
      }
    }
  });

  test("round-trips every source driver through the V5-only channel contract", () => {
    const snapshots = SOURCE_DRIVERS.map((driver) => {
      const channel = sourceDriverInitialChannelV5(driver, {
        id: `channel-${driver.id}`,
        name: driver.title,
        credentialRef: `credential://${driver.id}`,
        nodeId: `node-${driver.id}`,
        serverWsUrl: "wss://platform.example.test/supplier/ws",
        serverQuicUrl: "platform.example.test:443",
      });
      expect(() => validateChannelV5(channel)).not.toThrow();

      const serialized = serializeChannelV5(channel);
      expect(serialized).not.toHaveProperty("kind");
      expect(serialized).not.toHaveProperty("api_format");
      expect(serialized).not.toHaveProperty("supported_protocols");
      expect(serialized).not.toHaveProperty("upstream_base_url");
      expect(serialized).not.toHaveProperty("upstream_api_key");
      expect(serialized).not.toHaveProperty("public_model");
      expect(serialized).not.toHaveProperty("upstream_model");
      expect(serialized).not.toHaveProperty("surface_bindings");

      const reloaded = reloadChannelV5(JSON.parse(JSON.stringify(serialized)));
      expect(reloaded).toEqual(serialized);
      return reloaded;
    });

    expect(snapshots).toMatchSnapshot();
  });

  test("keeps LAN share channels local-only and derives every protocol surface from one address", () => {
    const channel = sourceDriverInitialChannelV5(sourceDriverById("lan_share"), {
      id: "office-share",
      name: "Office share",
      credentialRef: "credential://office-share",
    });
    channel.enabled = true;
    channel.share_enabled = true;
    const openAi = channel.surfaces.find((surface) => surface.surface === "open_ai");
    if (!openAi) throw new Error("missing LAN share OpenAI surface");
    openAi.base_url = "http://192.168.1.20:38787";

    const normalized = normalizeChannelV5(channel);

    expect(normalized.share_enabled).toBe(false);
    expect(Object.fromEntries(normalized.surfaces.map((surface) => [surface.surface, surface.base_url]))).toEqual({
      open_ai: "http://192.168.1.20:38787/v1",
      anthropic: "http://192.168.1.20:38787/anthropic",
      gemini: "http://192.168.1.20:38787/gemini",
    });
    expect(serializeChannelV5(normalized).share_enabled).toBe(false);
  });

  test("round-trips the configured default model independently from catalog order", () => {
    const channel = sourceDriverInitialChannelV5(sourceDriverById("custom_endpoint"), {
      id: "custom",
      name: "Custom",
      credentialRef: "credential://custom",
    });
    channel.models = ["first-model", "Chosen-Model"];
    channel.default_model = "chosen-model";

    const reloaded = reloadChannelV5(JSON.parse(JSON.stringify(serializeChannelV5(channel))));

    expect(reloaded.models).toEqual(["first-model", "Chosen-Model"]);
    expect(reloaded.default_model).toBe("Chosen-Model");
  });

  test("keeps an optional User-Agent profile without changing legacy channels", () => {
    const channel = sourceDriverInitialChannelV5(sourceDriverById("custom_endpoint"), {
      id: "custom-user-agent",
      name: "Custom User-Agent",
    });
    expect(serializeChannelV5(channel)).not.toHaveProperty("user_agent_profile");

    channel.user_agent_profile = "  OpenCode  ";
    const reloaded = reloadChannelV5(JSON.parse(JSON.stringify(serializeChannelV5(channel))));

    expect(reloaded.user_agent_profile).toBe("opencode");
  });

  test("persists the channel creation time and defaults legacy channels to zero", () => {
    const channel = sourceDriverInitialChannelV5(sourceDriverById("openai_api"), {
      id: "created-channel",
      name: "Created channel",
      createdAtUnixMs: 1_725_000_000_123,
    });
    const serialized = serializeChannelV5(channel);

    expect(serialized.created_at_unix_ms).toBe(1_725_000_000_123);

    const legacy = structuredClone(serialized) as Partial<typeof serialized>;
    delete legacy.created_at_unix_ms;
    expect(reloadChannelV5(legacy).created_at_unix_ms).toBe(0);
  });

  test("migrates former subscription-scoped capacity into channel-wide fields", () => {
    const legacy = sourceDriverInitialChannelV5(sourceDriverById("openai_subscription"), {
      id: "legacy-subscription",
      name: "Legacy subscription",
    });
    if (!legacy.subscription) throw new Error("missing subscription fixture");
    legacy.subscription.max_concurrency = 6;
    legacy.subscription.quota_reserve_percent = 12;
    const withoutChannelCapacity = structuredClone(legacy) as Partial<typeof legacy>;
    delete withoutChannelCapacity.max_concurrency;
    delete withoutChannelCapacity.quota_reserve_percent;

    const migrated = reloadChannelV5(withoutChannelCapacity);

    expect(migrated.max_concurrency).toBe(6);
    expect(migrated.quota_reserve_percent).toBe(12);
    expect(migrated.subscription?.max_concurrency).toBe(6);
    expect(migrated.subscription?.quota_reserve_percent).toBe(12);
  });

  test("serializes the top-level V5 config without the legacy supplier master", () => {
    const channel = sourceDriverInitialChannelV5(sourceDriverById("openai_api"), {
      id: "openai",
      name: "OpenAI",
      credentialRef: "credential://openai",
    });
    const serialized = serializeClientConfigV5({
      config_version: 4,
      listen: "127.0.0.1:38787",
      channels: [{ ...channel, kind: "legacy-kind" }],
      supplier: { kind: "legacy-supplier" },
    });

    expect(serialized.config_version).toBe(5);
    expect(serialized).not.toHaveProperty("supplier");
    expect(serialized.channels).toEqual([serializeChannelV5(channel)]);
  });

  test("keeps retained subscription bindings without URLs while HTTP authority stays strict", () => {
    const subscription = sourceDriverInitialChannelV5(sourceDriverById("openai_subscription"), {
      id: "subscription",
      name: "Subscription",
      credentialRef: "credential://subscription",
    });
    expect(normalizeChannelV5(subscription).surfaces[0].base_url).toBe("");

    const http = sourceDriverInitialChannelV5(sourceDriverById("openai_api"), {
      id: "http",
      name: "HTTP",
      credentialRef: "credential://http",
    });
    http.surfaces[0].base_url = "https://api.openai.com/v1?authority=evil.example";
    expect(() => normalizeChannelV5(http)).toThrow(/base URL|query|authority/i);
  });

  test("neutralizes retired subscription controls without changing models", () => {
    const channel = sourceDriverInitialChannelV5(sourceDriverById("openai_subscription"), {
      id: "subscription",
      name: "Subscription",
      credentialRef: "credential://subscription",
    });
    channel.models = ["Model-A", "Model-B"];
    channel.default_model = "Model-B";
    if (!channel.subscription) throw new Error("missing subscription fixture");
    channel.subscription.daily_request_limit = 123;
    channel.subscription.risk_note = "legacy risk note";
    channel.subscription.audit_enabled = true;

    const normalized = normalizeChannelV5(channel);

    expect(normalized.subscription).toEqual(expect.objectContaining({
      daily_request_limit: 0,
      risk_note: "",
      audit_enabled: false,
    }));
    expect(normalized.models).toEqual(["Model-A", "Model-B"]);
    expect(normalized.default_model).toBe("Model-B");
  });

  test("keeps channel default target and each surface preferred protocol independent", () => {
    const channel = sourceDriverInitialChannelV5(sourceDriverById("bedrock_mantle"), {
      id: "bedrock",
      name: "Bedrock",
      credentialRef: "credential://bedrock",
    });
    channel.default_target = { surface: "anthropic", protocol: "anthropic_messages" };

    const normalized = normalizeChannelV5(channel);
    expect(normalized.default_target).toEqual({ surface: "anthropic", protocol: "anthropic_messages" });
    expect(normalized.surfaces.find((surface) => surface.surface === "open_ai")?.protocols)
      .toEqual(expect.arrayContaining([
        expect.objectContaining({ protocol: "openai_responses", preferred: true }),
        expect.objectContaining({ protocol: "openai_chat", preferred: false }),
      ]));

    const changedPreferred = structuredClone(normalized);
    const openAi = changedPreferred.surfaces.find((surface) => surface.surface === "open_ai");
    if (!openAi) throw new Error("missing OpenAI surface fixture");
    openAi.protocols = openAi.protocols.map((binding) => ({
      ...binding,
      preferred: binding.protocol === "openai_chat",
    }));
    expect(normalizeChannelV5(changedPreferred).default_target).toEqual(normalized.default_target);
  });

  test("merges real V2 detection evidence without rebuilding immutable or legacy truth", () => {
    const channel = {
      ...sourceDriverInitialChannelV5(sourceDriverById("openai_api"), {
        id: "openai",
        name: "OpenAI",
        credentialRef: "credential://openai",
      }),
      kind: "must-stay",
      api_format: "openai_responses",
      supported_protocols: ["legacy-conflict"],
      upstream_model: "configured-upstream",
      public_model: "configured-public",
    };
    const detection = {
      models: ["Detected-B", "Detected-A"],
      surface_results: [{
        surface: "open_ai" as const,
        protocol: "openai_chat" as const,
        surface_verification: { state: "verified", checked_at_unix: 42, summary: "catalog ok" },
        protocol_verification: { state: "verified", checked_at_unix: 43, summary: "pair ok" },
        models: ["Detected-B", "Detected-A"],
        model_capability_evidence: [],
        detection_evidence: [],
        checks: [],
        warnings: [],
      }],
      model_capability_evidence: [{ protocol: "anthropic_messages", verification_state: "verified" }],
      detection_evidence: [{ name: "catalog", status: "ok" }],
      quota_status: "available",
      remaining_ratio: 0.75,
      quota_windows: [],
      checks: ["catalog ok"],
      warnings: [],
    };

    const merged = applyChannelDetectionV2(channel, detection);
    expect(merged).toMatchObject({
      source_driver: "openai_api",
      executor: { type: "http_surface" },
      kind: "must-stay",
      api_format: "openai_responses",
      supported_protocols: ["legacy-conflict"],
      upstream_model: "configured-upstream",
      public_model: "configured-public",
      models: ["Detected-B", "Detected-A"],
      model_capability_evidence: detection.model_capability_evidence,
      detection_evidence: detection.detection_evidence,
    });
    expect(merged.surfaces[0].protocols.find((binding) => binding.protocol === "openai_chat")?.verification)
      .toEqual(detection.surface_results[0].protocol_verification);
    expect(merged.surfaces[0].protocols.find((binding) => binding.protocol === "openai_responses")?.verification.state)
      .toBe("declared");
  });

  test("never promotes model capability profiles into native protocol verification", () => {
    const channel = sourceDriverInitialChannelV5(sourceDriverById("anthropic_api"), {
      id: "anthropic",
      name: "Anthropic",
      credentialRef: "credential://anthropic",
    });
    const merged = applyChannelDetectionV2(channel, {
      models: ["claude-test"],
      surface_results: [],
      model_capability_evidence: [{ protocol: "anthropic_messages", verification_state: "verified" }],
      detection_evidence: [],
      quota_status: "",
      remaining_ratio: 0,
      quota_windows: [],
      checks: [],
      warnings: [],
    });

    expect(merged.surfaces[0].protocols[0].verification.state).toBe("declared");
  });
});
