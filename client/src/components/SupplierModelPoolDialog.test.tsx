// @vitest-environment jsdom

import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, test, vi } from "vitest";

import {
  resolveSupplierModelPoolDetails,
  SupplierModelPoolDialog,
} from "./SupplierModelPoolDialog";

describe("resolveSupplierModelPoolDetails", () => {
  test("old context-hint reports and new short reports still belong to the observed model", () => {
    for (const report of ["vendor/claude[1M]", "claude"]) {
      expect(resolveSupplierModelPoolDetails({observedModels:["vendor/claude", "vendor/claude[1m]"], routeStatus:{accepted_models:[report]}, platformRegistered:true})).toEqual({total:1, accepted:["claude"], unsupported:[], unreported:[]});
    }
  });
  test("keeps the first observed short-name source across admission groups", () => {
    const observedModels = ["vendor-a/model", "vendor-b/model", "vendor-c/other:free"];
    const routeStatus = {
      accepted_models: ["vendor-a/model"],
      unsupported_models: ["vendor-b/model"],
    };
    expect(resolveSupplierModelPoolDetails({ observedModels, routeStatus, platformRegistered: true })).toEqual({
      total: 2,
      accepted: ["model"],
      unsupported: [],
      unreported: ["other:free"],
    });
    expect(observedModels).toEqual(["vendor-a/model", "vendor-b/model", "vendor-c/other:free"]);
    expect(routeStatus.accepted_models).toEqual(["vendor-a/model"]);
    expect(routeStatus.unsupported_models).toEqual(["vendor-b/model"]);
  });

  test("uses the explicit platform admission response when it is available", () => {
    expect(resolveSupplierModelPoolDetails({
      observedModels: ["model-a", "model-b", "model-c"],
      routeStatus: {
        accepted_models: ["model-a", "model-b"],
        unsupported_models: ["model-c"],
      },
      platformRegistered: true,
    })).toEqual({
      total: 3,
      accepted: ["model-a", "model-b"],
      unsupported: ["model-c"],
      unreported: [],
    });
  });

  test("treats observed models as accepted after a clean platform registration", () => {
    expect(resolveSupplierModelPoolDetails({
      observedModels: ["model-a", "model-b"],
      platformRegistered: true,
    })).toEqual({
      total: 2,
      accepted: ["model-a", "model-b"],
      unsupported: [],
      unreported: [],
    });
  });

  test("keeps local models unreported before the platform acknowledges them", () => {
    expect(resolveSupplierModelPoolDetails({
      observedModels: ["model-a", "model-b"],
      platformRegistered: false,
    })).toEqual({
      total: 2,
      accepted: [],
      unsupported: [],
      unreported: ["model-a", "model-b"],
    });
  });
});

describe("SupplierModelPoolDialog", () => {
  test.each(["OpenRouter", "OpenAI", "Claude", "Antigravity", "Custom endpoint"])(
    "%s displays and copies only model names within every admission group",
    (channelName) => {
      const onCopy = vi.fn();
      render(
        <SupplierModelPoolDialog
          channelName={channelName}
          observedModels={["vendor/Model-C[1M]", "vendor/Model-B", "vendor/Model-A"]}
          routeStatus={{ accepted_models: ["vendor/Model-A"], unsupported_models: ["vendor/Model-B"] }}
          platformRegistered
          onClose={vi.fn()}
          onCopy={onCopy}
        />,
      );
      const details = (screen.getByRole("textbox") as HTMLTextAreaElement).value;
      const sections = details.split("\n\n").map((section) => section.split("\n"));
      expect(sections.map(([heading]) => heading)).toEqual([
        expect.stringMatching(/ \(1\)$/),
        expect.stringMatching(/ \(1\)$/),
        expect.stringMatching(/ \(1\)$/),
      ]);
      expect(sections.map(([, ...models]) => models)).toEqual([["model-a"], ["model-b"], ["model-c"]]);
      fireEvent.click(screen.getByRole("button", { name: /Copy details|复制详情/ }));
      expect(onCopy).toHaveBeenCalledWith(details, expect.any(String));
    },
  );

  test("displays and copies normalized names in the admission details", () => {
    const onCopy = vi.fn();
    render(
      <SupplierModelPoolDialog
        channelName="OpenRouter"
        observedModels={["qwen/qwen3.8-27b", "thinkingmachines/inkling:free"]}
        routeStatus={{ accepted_models: ["qwen/qwen3.8-27b"], unsupported_models: ["thinkingmachines/inkling:free"] }}
        platformRegistered
        onClose={vi.fn()}
        onCopy={onCopy}
      />,
    );
    const details = (screen.getByRole("textbox") as HTMLTextAreaElement).value;
    expect(details).toContain("qwen3.8-27b");
    expect(details).toContain("inkling:free");
    expect(details).not.toContain("/");
    fireEvent.click(screen.getByRole("button", { name: /Copy details|复制详情/ }));
    expect(onCopy).toHaveBeenCalledWith(details, expect.any(String));
  });

  test("shows copyable grouped details and closes with Escape", () => {
    const onClose = vi.fn();
    const onCopy = vi.fn();
    render(
      <SupplierModelPoolDialog
        channelName="Antigravity"
        observedModels={["model-a", "model-b"]}
        routeStatus={{
          accepted_models: ["model-a"],
          unsupported_models: ["model-b"],
          catalog_release_id: "catalog-7",
        }}
        platformRegistered
        onClose={onClose}
        onCopy={onCopy}
      />,
    );

    const details = screen.getByRole("textbox") as HTMLTextAreaElement;
    expect(details.value).toContain("model-a");
    expect(details.value).toContain("model-b");

    fireEvent.click(screen.getByRole("button", { name: /Copy details|复制详情/ }));
    expect(onCopy).toHaveBeenCalledWith(
      expect.stringContaining("model-a"),
      expect.any(String),
    );

    fireEvent.click(screen.getByRole("dialog"));
    expect(onClose).not.toHaveBeenCalled();

    fireEvent.keyDown(document, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
