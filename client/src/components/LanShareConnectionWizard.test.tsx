// @vitest-environment jsdom

import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterAll, beforeEach, describe, expect, test, vi } from "vitest";
import { changeAppLanguage } from "../i18n";
import { LanShareConnectionWizard } from "./LanShareConnectionWizard";

const originalClipboard = navigator.clipboard;

describe("LanShareConnectionWizard", () => {
  beforeEach(async () => {
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: undefined });
    await changeAppLanguage("zh-CN");
  });

  afterAll(() => {
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: originalClipboard });
  });

  test("waits for Start setup after parsing a complete connection block", async () => {
    const user = userEvent.setup();
    const onImport = vi.fn();
    render(
      <LanShareConnectionWizard
        onClose={vi.fn()}
        onImport={onImport}
        onSkip={vi.fn()}
      />,
    );

    expect(screen.getByRole("dialog", { name: "添加局域网共享渠道" })).toBeInTheDocument();
    const connectionText = [
      "CONST API 局域网共享渠道",
      "渠道名称: 局域网共享 · 192.168.1.20",
      "Base URL: http://192.168.1.20:38788/v1",
      "API Key: cst-lan-alice",
      "使用方法:",
      "3. 确认自动填写的内容，然后点击“验证并保存”，成功后点击“启用渠道”。",
    ].join("\n");
    fireEvent.paste(screen.getByLabelText("粘贴连接信息"), {
      clipboardData: {
        getData: () => connectionText,
      },
    });

    expect(onImport).not.toHaveBeenCalled();
    expect(screen.getByLabelText("粘贴连接信息")).toHaveValue(connectionText);
    expect(screen.getByRole("status")).toHaveTextContent("请点击“开始配置”继续");

    await user.click(screen.getByRole("button", { name: "开始配置" }));
    expect(onImport).toHaveBeenCalledWith({
      channelName: "局域网共享 · 192.168.1.20",
      baseUrl: "http://192.168.1.20:38788/v1",
      apiKey: "cst-lan-alice",
    });
  });

  test("keeps Start setup disabled until connection information is recognized", () => {
    render(
      <LanShareConnectionWizard
        onClose={vi.fn()}
        onImport={vi.fn()}
        onSkip={vi.fn()}
      />,
    );

    expect(screen.getByRole("button", { name: "开始配置" })).toBeDisabled();
  });

  test("loads valid clipboard connection details without leaving the wizard", async () => {
    const connectionText = [
      "CONST API 局域网共享渠道",
      "渠道名称: 局域网共享 · 192.168.1.20",
      "Base URL: http://192.168.1.20:38788/v1",
      "API Key: cst-lan-alice",
    ].join("\n");
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { readText: vi.fn().mockResolvedValue(connectionText) },
    });
    const onImport = vi.fn();

    render(
      <LanShareConnectionWizard
        onClose={vi.fn()}
        onImport={onImport}
        onSkip={vi.fn()}
      />,
    );

    await waitFor(() => expect(screen.getByLabelText("粘贴连接信息")).toHaveValue(connectionText));
    expect(screen.getByRole("button", { name: "开始配置" })).toBeEnabled();
    expect(screen.getByRole("dialog", { name: "添加局域网共享渠道" })).toBeVisible();
    expect(onImport).not.toHaveBeenCalled();
  });

  test("allows entering the LAN channel form without pasted information", async () => {
    const user = userEvent.setup();
    const onSkip = vi.fn();
    render(
      <LanShareConnectionWizard
        onClose={vi.fn()}
        onImport={vi.fn()}
        onSkip={onSkip}
      />,
    );

    await user.click(screen.getByRole("button", { name: "跳过，直接配置" }));
    expect(onSkip).toHaveBeenCalledOnce();
  });
});
