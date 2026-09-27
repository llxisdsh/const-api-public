// @vitest-environment jsdom

import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, test, vi } from "vitest";
import { changeAppLanguage } from "../i18n";
import { LanShareConnectionImport } from "./LanShareConnectionImport";

describe("LanShareConnectionImport", () => {
  beforeEach(async () => {
    await changeAppLanguage("zh-CN");
  });

  test("fills all LAN channel fields from one paste", () => {
    const onParsed = vi.fn();
    render(<LanShareConnectionImport onParsed={onParsed} />);

    expect(screen.getByLabelText("粘贴连接信息")).toHaveAttribute("rows", "16");

    fireEvent.paste(screen.getByLabelText("粘贴连接信息"), {
      clipboardData: {
        getData: () => [
          "CONST API 局域网共享渠道",
          "渠道名称: 局域网共享 · 192.168.1.20",
          "Base URL: http://192.168.1.20:38788/v1",
          "API Key: cst-lan-alice",
        ].join("\n"),
      },
    });

    expect(onParsed).toHaveBeenCalledWith({
      channelName: "局域网共享 · 192.168.1.20",
      baseUrl: "http://192.168.1.20:38788/v1",
      apiKey: "cst-lan-alice",
    });
    expect(screen.getByRole("status")).toHaveTextContent("已识别渠道名称、Base URL 和 API Key");
  });

  test("does not import unrelated clipboard text", () => {
    const onParsed = vi.fn();
    render(<LanShareConnectionImport onParsed={onParsed} />);

    fireEvent.paste(screen.getByLabelText("粘贴连接信息"), {
      clipboardData: { getData: () => "API Key: unrelated" },
    });

    expect(onParsed).toHaveBeenCalledWith(null);
    expect(screen.getByRole("status")).toHaveTextContent("没有识别到完整的局域网共享连接信息");
  });
});
