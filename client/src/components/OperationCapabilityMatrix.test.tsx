import { render, screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { sourceDriverById } from "../sourceDrivers";
import { OperationCapabilityMatrix } from "./OperationCapabilityMatrix";

function capabilityRow(name: string): HTMLElement {
  const row = screen.getByText(name).closest(".capability-summary-row");
  if (!(row instanceof HTMLElement)) throw new Error(`Capability row not found: ${name}`);
  return row;
}

describe("OperationCapabilityMatrix", () => {
  it("shows only usable Antigravity subscription capabilities in user language", () => {
    render(<OperationCapabilityMatrix driver={sourceDriverById("gemini_subscription")} />);

    expect(within(capabilityRow("模型列表")).getByText("支持")).toBeInTheDocument();
    expect(within(capabilityRow("内容生成、流式与令牌计数")).getByText("支持")).toBeInTheDocument();
    expect(screen.queryByText("嵌入向量")).not.toBeInTheDocument();
    expect(screen.queryByText("gemini.embed_content")).not.toBeInTheDocument();
    expect(screen.queryByText(/C2|A \/ S \/ P/)).not.toBeInTheDocument();
  });

  it("summarizes official API coverage as supported or limited", () => {
    render(<OperationCapabilityMatrix driver={sourceDriverById("openai_api")} />);

    expect(within(capabilityRow("Responses 与状态管理")).getByText("支持")).toBeInTheDocument();
    expect(within(capabilityRow("语音与音频")).getByText("支持")).toBeInTheDocument();
    expect(within(capabilityRow("向量库与文件检索")).getByText("支持")).toBeInTheDocument();
    expect(screen.queryByText("组织与项目管理")).not.toBeInTheDocument();
    expect(screen.queryByText("openai.responses")).not.toBeInTheDocument();
    expect(screen.queryByText(/C0|L2|待渠道验证/)).not.toBeInTheDocument();
  });

  it("does not claim unsupported subscription resources", () => {
    render(<OperationCapabilityMatrix driver={sourceDriverById("openai_subscription")} />);

    expect(screen.getByRole("heading", { name: "可处理的 API 请求" })).toBeInTheDocument();
    expect(within(capabilityRow("Responses 与状态管理")).getByText("有限支持")).toBeInTheDocument();
    expect(within(capabilityRow("Responses 与状态管理"))
      .getByText("支持核心调用，部分扩展操作暂不可用")).toBeInTheDocument();
    expect(within(capabilityRow("Chat Completions 与已存储对话"))
      .getByText("有限支持")).toBeInTheDocument();
    expect(within(capabilityRow("Chat Completions 与已存储对话"))
      .getByText("支持核心调用，部分扩展操作暂不可用")).toBeInTheDocument();
    expect(screen.queryByText("嵌入向量")).not.toBeInTheDocument();
    expect(screen.queryByText("图片输入与生成")).not.toBeInTheDocument();
  });
});
