import { act, fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { changeAppLanguage } from "../i18n";
import { SupplierChannelChecks } from "./SupplierChannelChecks";

const props = () => ({
  checks: [], busy: false, canSupply: true, saving: false, testing: false,
  model: "custom-model", prompt: "Reply OK", onPromptChange: vi.fn(), onFullCheck: vi.fn(), onTest: vi.fn(),
});

describe("SupplierChannelChecks", () => {
  it("shows the same full-check, result and current-model test sections for a custom channel", async () => {
    const input = props();
    const { container } = render(<SupplierChannelChecks {...input}><div>Test response</div></SupplierChannelChecks>);
    const full = screen.getByRole("button", { name: "完整检查并保存" });
    const results = screen.getByText("每组测试一个模型，并检查协议能力");
    const test = screen.getByRole("button", { name: "测试" });
    expect(results.compareDocumentPosition(full) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(results.compareDocumentPosition(test) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(screen.getByText("custom-model")).toHaveAttribute("title", "当前模型：custom-model");
    expect(screen.getByText("custom-model").parentElement).toContainElement(test);
    expect(container.querySelector(".supplier-test-panel header")).toBeNull();
    expect(screen.getByText("尚未完整检查")).toBeInTheDocument();
    expect(screen.queryByText(/上游额度/)).not.toBeInTheDocument();
    expect(screen.getByText("Test response").closest(".supplier-test-panel")).not.toBeNull();
    expect(container.querySelectorAll(".supplier-model-test-controls > button")).toHaveLength(1);
    await act(async () => { fireEvent.click(full); });
    expect(input.onFullCheck).toHaveBeenCalledOnce();
    expect(input.onTest).not.toHaveBeenCalled();
    fireEvent.change(screen.getByRole("textbox", { name: "测试内容" }), { target: { value: "New prompt" } });
    expect(input.onPromptChange).toHaveBeenCalledWith("New prompt");
    fireEvent.click(test);
    expect(input.onTest).toHaveBeenCalledOnce();
    expect(full).toHaveAttribute("type", "button");
    expect(test).toHaveAttribute("type", "button");
  });

  it.each([{ busy: true, canSupply: true }, { busy: false, canSupply: false }])(
    "keeps both actions visible but unavailable when busy or unconfigured: %j", (state) => {
      const input = { ...props(), ...state };
      render(<SupplierChannelChecks {...input} />);
      for (const button of screen.getAllByRole("button")) {
        expect(button).toBeDisabled();
        fireEvent.click(button);
      }
      expect(input.onFullCheck).not.toHaveBeenCalled();
      expect(input.onTest).not.toHaveBeenCalled();
    },
  );

  it("shows the current-model test's running state", () => {
    render(<SupplierChannelChecks {...props()} busy testing />);
    expect(screen.getByRole("button", { name: "测试中…" })).toBeDisabled();
  });

  it("combines results and date without making disclosure trigger a paid check", async () => {
    const input = {
      ...props(),
      checks: [{ name: "availability_group", capability: "other", status: "available", checked_at_unix: 1789110732 }],
    };
    const { container, rerender } = render(<SupplierChannelChecks {...input} />);
    const summary = screen.getByRole("button", { name: /^每组测试一个模型/ });
    const full = screen.getByRole("button", { name: "完整检查并保存" });
    expect(container.querySelectorAll("h4")).toHaveLength(0);
    expect(summary.closest(".supplier-check-actions")!.querySelectorAll("p")).toHaveLength(0);
    expect(summary).toContainElement(container.querySelector("time"));
    expect(summary.closest(".supplier-check-actions")).toContainElement(full);
    expect(summary.querySelector("button")).toBeNull();
    expect(screen.queryByText(/上游额度/)).not.toBeInTheDocument();
    fireEvent.click(summary);
    expect(summary).toHaveAttribute("aria-expanded", "true");
    expect(input.onFullCheck).not.toHaveBeenCalled();
    expect(input.onTest).not.toHaveBeenCalled();
    await act(async () => { fireEvent.click(full); });
    expect(input.onFullCheck).toHaveBeenCalledOnce();
    expect(summary).toHaveAttribute("aria-expanded", "true");
    rerender(<SupplierChannelChecks {...input} busy saving />);
    expect(full).toBeDisabled();
    expect(summary).toBeEnabled();
    fireEvent.click(summary);
    expect(summary).toHaveAttribute("aria-expanded", "false");
  });

  it.each(["available", "suspended"])("opens %s results only after a manual full check completes", async (status) => {
    let complete!: () => void;
    const pending = new Promise<void>((resolve) => { complete = resolve; });
    const input = {
      ...props(),
      checks: [{ name: "availability_group", capability: "other", status, checked_at_unix: 1789110732 }],
      onFullCheck: vi.fn(() => pending),
    };
    const { rerender } = render(<SupplierChannelChecks {...input} />);
    const summary = screen.getByRole("button", { name: /^每组测试一个模型/ });
    expect(summary).toHaveAttribute("aria-expanded", "false");
    // Ordinary saves and newer background evidence must not open the results.
    rerender(<SupplierChannelChecks {...input} busy saving />);
    rerender(<SupplierChannelChecks {...input} checks={input.checks.map((check) => ({ ...check, checked_at_unix: 1789110733 }))} />);
    expect(summary).toHaveAttribute("aria-expanded", "false");
    fireEvent.click(screen.getByRole("button", { name: "测试" }));
    expect(summary).toHaveAttribute("aria-expanded", "false");
    fireEvent.click(screen.getByRole("button", { name: "完整检查并保存" }));
    expect(summary).toHaveAttribute("aria-expanded", "false");
    await act(async () => { complete(); await pending; });
    expect(summary).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByText(status === "available" ? "通过" : "暂停供应")).toBeVisible();
    expect(input.onFullCheck).toHaveBeenCalledOnce();
    fireEvent.click(summary);
    expect(summary).toHaveAttribute("aria-expanded", "false");
    // A later manual run opens the results again even if the content is unchanged.
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "完整检查并保存" })); });
    expect(summary).toHaveAttribute("aria-expanded", "true");
  });

  it("starts collapsed on reopening and ignores an old dialog's pending completion", async () => {
    let complete!: () => void;
    const pending = new Promise<void>((resolve) => { complete = resolve; });
    const input = {
      ...props(),
      checks: [{ name: "availability_group", capability: "other", status: "available" }],
      onFullCheck: vi.fn(() => pending),
    };
    const first = render(<SupplierChannelChecks {...input} />);
    fireEvent.click(screen.getByRole("button", { name: /^每组测试一个模型/ }));
    fireEvent.click(screen.getByRole("button", { name: "完整检查并保存" }));
    first.unmount();
    const reopened = render(<SupplierChannelChecks key="channel-a" {...input} />);
    expect(screen.getByRole("button", { name: /^每组测试一个模型/ })).toHaveAttribute("aria-expanded", "false");
    await act(async () => { complete(); await pending; });
    expect(screen.getByRole("button", { name: /^每组测试一个模型/ })).toHaveAttribute("aria-expanded", "false");
    fireEvent.click(screen.getByRole("button", { name: /^每组测试一个模型/ }));
    reopened.rerender(<SupplierChannelChecks key="channel-b" {...input} />);
    expect(screen.getByRole("button", { name: /^每组测试一个模型/ })).toHaveAttribute("aria-expanded", "false");
  });

  it("localizes the full panel in English", async () => {
    await changeAppLanguage("en-US");
    try {
      render(<SupplierChannelChecks {...props()} />);
      expect(screen.getByRole("button", { name: "Test" })).toBeInTheDocument();
      expect(screen.getByText("Test one model per group and check protocol capabilities")).toBeInTheDocument();
      expect(screen.getByText("No full check yet")).toBeInTheDocument();
    } finally { await changeAppLanguage("zh-CN"); }
  });
});
