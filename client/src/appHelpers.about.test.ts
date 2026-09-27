import { describe, expect, it } from "vitest";
import {
  aboutServerLabel,
  aboutUpdateHint,
  emptyConfig,
  endpointRegistrySourceLabel,
  endpointRegistrySourceNote,
  modelCatalogVersionDetails,
  modelCatalogVersionHint,
  modelCatalogVersionLabel,
  modelCatalogSourceLabel,
  modelCatalogSourceNote,
  updateSourceLabel,
  updateSourceNote,
} from "./appHelpers";
import type { AppMetaState, ModelCatalogVersionInfo } from "./appTypes";

const publicCatalog: ModelCatalogVersionInfo = {
  release_id: "catalog-v10-2026-07-27",
  model_version_id: "models-v7-2026-07-27",
  compatibility_version_id: "compat-v10-2026-07-27",
  sequence: 10,
  source: "public",
};

const baseMeta: AppMetaState = {
  currentVersion: "0.1.14",
  updateSources: [],
  endpointSources: [],
  endpointStatus: "idle",
};

describe("about concise runtime information", () => {
  it("shows the observed server version without inventing one for older servers", () => {
    const status = { running: true, listen: "127.0.0.1:38787", active_endpoint: "cn-legacy" };
    expect(aboutServerLabel({ ...status, platform_server_version: "0.1.41" }, emptyConfig)).toBe("v0.1.41 · cn-legacy");
    expect(aboutServerLabel({ ...status, platform_server_version: "v0.1.41" }, emptyConfig)).toBe("v0.1.41 · cn-legacy");
    expect(aboutServerLabel(status, emptyConfig)).toBe("cn-legacy");
    expect(aboutServerLabel({ ...status, platform_server_version: "" }, emptyConfig)).toBe("cn-legacy");
  });

  it("hides update timestamps but retains actionable update hints", () => {
    const checkedAt = "2026/9/8 18:50:08";
    expect(aboutUpdateHint({ status: "idle", checkedAt })).toBe("");
    expect(aboutUpdateHint({ status: "idle" }, checkedAt)).toBe("");
    expect(aboutUpdateHint({ status: "idle" })).toBe("点击右上角检查更新");
    expect(aboutUpdateHint({ status: "ready", version: "0.1.52", checkedAt })).toBe("重启后完成更新");
    expect(aboutUpdateHint({ status: "ready_waiting_idle", waitingReason: "等待请求结束" })).toBe("等待请求结束");
    expect(aboutUpdateHint({ status: "error", error: "更新源不可达", checkedAt })).toBe("更新源不可达");
  });
});

describe("about model catalog version", () => {
  it("shows the active model version and its actual source", () => {
    const meta = { ...baseMeta, modelCatalog: publicCatalog };

    expect(modelCatalogVersionLabel(meta.modelCatalog)).toBe("models-v7-2026-07-27");
    expect(modelCatalogVersionHint(meta)).toBe("兼容 compat-v10-2026-07-27");
    expect(modelCatalogSourceLabel(meta.modelCatalog)).toBe("2 个通道");
    expect(modelCatalogSourceNote(meta)).toBe("最近使用：首选");
    expect(modelCatalogVersionDetails(meta.modelCatalog)).toBe([
      "模型版本：models-v7-2026-07-27",
      "目录版本：catalog-v10-2026-07-27",
      "兼容版本：compat-v10-2026-07-27",
      "目录序号：10",
    ].join("\n"));
  });

  it("distinguishes packaged development metadata from published metadata", () => {
    const meta = {
      ...baseMeta,
      modelCatalog: {
        ...publicCatalog,
        model_version_id: "models-v8-dev",
        source: "packaged",
      },
    };

    expect(modelCatalogVersionLabel(meta.modelCatalog)).toBe("models-v8-dev");
    expect(modelCatalogSourceLabel(meta.modelCatalog)).toBe("1 个通道");
    expect(modelCatalogSourceNote(meta)).toBe("最近使用：内置");
  });

  it("keeps a valid version visible when a later refresh fails", () => {
    const meta = {
      ...baseMeta,
      modelCatalog: publicCatalog,
      modelCatalogLastError: "command unavailable",
    };

    expect(modelCatalogVersionLabel(meta.modelCatalog)).toBe("models-v7-2026-07-27");
    expect(modelCatalogVersionHint(meta)).toBe("兼容 compat-v10-2026-07-27");
    expect(modelCatalogSourceLabel(meta.modelCatalog)).toBe("2 个通道");
    expect(modelCatalogSourceNote(meta)).toBe("最近使用：首选 · 刷新失败");
  });

  it("distinguishes an initial read failure from a later refresh failure", () => {
    expect(modelCatalogSourceLabel()).toBe("读取中");
    expect(modelCatalogSourceNote({
      ...baseMeta,
      modelCatalogLastError: "command unavailable",
    })).toBe("读取失败");
  });
});

describe("about endpoint source", () => {
  it("uses the same lightweight source label as the update source", () => {
    const meta = {
      ...baseMeta,
      endpointSources: [
        "https://const.tos-cn-shanghai.volces.com/registry/endpoints.json",
        "https://github.com/llxisdsh/const-api-public/releases/download/endpoint-registry/endpoints.json",
      ],
      endpointSource: "https://github.com/llxisdsh/const-api-public/releases/download/endpoint-registry/endpoints.json",
      endpointStatus: "success" as const,
    };

    expect(endpointRegistrySourceLabel(meta, emptyConfig)).toBe("2 个通道");
    expect(endpointRegistrySourceNote(meta)).toBe("最近使用：备用");
  });

  it("shows only a short exceptional state beside the source", () => {
    expect(endpointRegistrySourceNote({
      ...baseMeta,
      endpointStatus: "error",
      endpointLastError: "network details stay in diagnostics",
    })).toBe("刷新失败");
  });
});

describe("about update source", () => {
  const sources = [
    "https://const.tos-cn-shanghai.volces.com/client/latest.json",
    "https://github.com/llxisdsh/const-api-public/releases/latest/download/latest.json",
  ];

  it("shows automatic selection before the first check", () => {
    const meta = { ...baseMeta, updateSources: sources };
    expect(updateSourceLabel(meta)).toBe("2 个通道");
    expect(updateSourceNote(meta)).toBe("待检测");
  });

  it("shows the selected transport and its fallback", () => {
    const meta = { ...baseMeta, updateSources: sources, updateSource: sources[0] };
    expect(updateSourceLabel(meta)).toBe("2 个通道");
    expect(updateSourceNote(meta)).toBe("最近使用：首选");
  });

  it("identifies when the fallback transport was selected", () => {
    const meta = { ...baseMeta, updateSources: sources, updateSource: sources[1] };
    expect(updateSourceNote(meta)).toBe("最近使用：备用");
  });
});

describe("about model catalog delivery source", () => {
  it("prefers the actual mirror over the logical public catalog label", () => {
    expect(modelCatalogSourceLabel({
      ...publicCatalog,
      delivery_source: "https://const.tos-cn-shanghai.volces.com/catalog-registry/catalog.json",
    })).toBe("2 个通道");
  });

  it("shows the verified international catalog as the fallback", () => {
    const sources = [
      "https://const.tos-cn-shanghai.volces.com/catalog-registry/catalog.json",
      "https://github.com/llxisdsh/const-api-public/releases/download/catalog-registry/catalog.json",
    ];
    expect(modelCatalogSourceNote({
      ...baseMeta,
      modelCatalog: {
        ...publicCatalog,
        delivery_source: sources[0],
        delivery_sources: sources,
      },
    })).toBe("最近使用：首选");
  });

  it("distinguishes a cached catalog from the bundled catalog", () => {
    expect(modelCatalogSourceNote({
      ...baseMeta,
      modelCatalog: {
        ...publicCatalog,
        delivery_source: "local-cache",
      },
    })).toBe("最近使用：缓存");
  });
});
