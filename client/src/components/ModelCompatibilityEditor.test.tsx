import { fireEvent, render, screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { ModelCompatibilityGroup, ModelCompatibilityPolicyState } from "../appTypes";
import {
  compatibilityCatalogCandidates,
  ModelCompatibilityEditor,
} from "./ModelCompatibilityEditor";

const groups: ModelCompatibilityGroup[] = [{
  id: "apex",
  label: "T0",
  aliases: ["default", "apex"],
  models: ["claude-fable-5", "gpt-5.6-sol"],
}];

function readyPolicy(
  overrides: Partial<ModelCompatibilityPolicyState> = {},
): ModelCompatibilityPolicyState {
  return {
    status: "ready",
    aliases: {},
    modelGroups: groups,
    templateModelGroups: groups,
    catalogModels: groups[0].models.map((id) => ({ id })),
    platformAvailableModels: [],
    localAvailableModels: [],
    source: "client",
    customized: false,
    platformSyncAvailable: false,
    syncStatus: "local",
    ...overrides,
  };
}

function renderEditor(policy: ModelCompatibilityPolicyState, onReset = vi.fn()) {
  render(
    <ModelCompatibilityEditor
      enabled
      policy={policy}
      draft={policy.modelGroups}
      setDraft={vi.fn()}
      saving={false}
      onClose={vi.fn()}
      onRefresh={vi.fn()}
      onSave={vi.fn()}
      onReset={onReset}
    />,
  );
  return onReset;
}

describe("ModelCompatibilityEditor reset", () => {
  it("allows a local non-customized cache to reset to the active shared catalog", () => {
    const onReset = renderEditor(readyPolicy());
    const reset = screen.getByRole("button", { name: "恢复当前默认" });

    expect(reset).toBeEnabled();
    fireEvent.click(reset);
    expect(onReset).toHaveBeenCalledOnce();
  });

  it("keeps reset available for an unchanged synchronized baseline", () => {
    renderEditor(readyPolicy({
      source: "server",
      platformSyncAvailable: true,
      syncStatus: "synced",
    }));

    expect(screen.getByRole("button", { name: "恢复当前默认" })).toBeEnabled();
  });
});

describe("ModelCompatibilityEditor candidates", () => {
  it("adds the union of configured local models without duplicating catalog entries", () => {
    const candidates = compatibilityCatalogCandidates(
      [{ id: "gpt-5.6-sol", vendor: "openai" }],
      [" gpt-5.6-sol ", "custom/local-gpt-6", "CUSTOM/local-gpt-6"],
    );

    expect(candidates.map((model) => model.id)).toEqual([
      "gpt-5.6-sol",
      "custom/local-gpt-6",
    ]);
    expect(candidates[1]).toMatchObject({ vendor: "local" });
  });
});

describe("ModelCompatibilityEditor system baseline", () => {
  it("renders system models as fixed without move or remove controls", () => {
    renderEditor(readyPolicy());

    for (const model of groups[0].models) {
      const row = screen.getByText(model).closest(".compatibility-model-row");
      expect(row).not.toBeNull();
      expect(within(row as HTMLElement).getByLabelText("系统模型固定")).toBeInTheDocument();
      expect(within(row as HTMLElement).queryByRole("button")).not.toBeInTheDocument();
    }
  });

  it("keeps edit controls for user-added models", () => {
    const customModel = "custom/local-gpt-6";
    renderEditor(readyPolicy({
      modelGroups: [{
        ...groups[0],
        models: [...groups[0].models, customModel],
      }],
      catalogModels: [
        ...groups[0].models.map((id) => ({ id })),
        { id: customModel, vendor: "local" },
      ],
      customized: true,
      source: "local_user",
    }));

    const row = screen.getByText(customModel).closest(".compatibility-model-row");
    expect(row).not.toBeNull();
    expect(within(row as HTMLElement).queryByLabelText("系统模型固定")).not.toBeInTheDocument();
    expect(within(row as HTMLElement).getAllByRole("button")).toHaveLength(3);
    expect(within(row as HTMLElement).getByTitle("移除")).toBeEnabled();
  });
});

describe("ModelCompatibilityEditor groups", () => {
  it("uses an explicit disclosure control for each compatibility tier", () => {
    renderEditor(readyPolicy());

    const toggle = screen.getByRole("button", { name: /T0/ });
    const bodyId = toggle.getAttribute("aria-controls");
    expect(bodyId).toBeTruthy();
    expect(toggle).toHaveAttribute("aria-expanded", "true");
    expect(document.getElementById(bodyId as string)).not.toHaveAttribute("hidden");

    fireEvent.click(toggle);
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(document.getElementById(bodyId as string)).toHaveAttribute("hidden");

    fireEvent.click(toggle);
    expect(toggle).toHaveAttribute("aria-expanded", "true");
    expect(document.getElementById(bodyId as string)).not.toHaveAttribute("hidden");
  });
});
