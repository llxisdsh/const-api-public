import { render, screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { CapabilityProfileList } from "./AppControls";

function featureRow(name: string): HTMLElement {
  const row = screen.getByText(name).closest(".capability-row");
  if (!(row instanceof HTMLElement)) throw new Error(`Capability feature not found: ${name}`);
  return row;
}

describe("CapabilityProfileList", () => {
  it("shows functional support without exposing protocol evidence", () => {
    render(<CapabilityProfileList profiles={[
      {
        protocol: "openai_responses",
        stream_sse: true,
        tool_calls: true,
        tool_choice: true,
        parallel_tool_calls: true,
        vision: true,
        verification_state: "declared",
      },
      {
        protocol: "anthropic_messages",
        audio_input: true,
        cache_control: true,
        verification_state: "verified",
      },
      {
        protocol: "gemini_native",
        video_output: true,
        verification_state: "failed",
      },
      {
        protocol: "gemini_native",
        file_output: true,
        verification_state: "rejected",
      },
    ]} />);

    expect(screen.getByRole("heading", { name: "主要能力" })).toBeInTheDocument();
    expect(within(featureRow("流式响应")).getByText("支持")).toBeInTheDocument();
    expect(within(featureRow("工具调用")).getByText("支持")).toBeInTheDocument();
    expect(within(featureRow("指定与并行工具")).getByText("支持")).toBeInTheDocument();
    expect(within(featureRow("图片理解（Vision）")).getByText("支持")).toBeInTheDocument();
    expect(within(featureRow("音频输入")).getByText("支持")).toBeInTheDocument();
    expect(within(featureRow("提示词缓存")).getByText("支持")).toBeInTheDocument();
    expect(screen.queryByText("视频输出")).not.toBeInTheDocument();
    expect(screen.queryByText("文件输出")).not.toBeInTheDocument();
    expect(screen.queryByText(/openai_responses|anthropic_messages|已验证|已声明/)).not.toBeInTheDocument();
  });

  it("separates agent-local file access from standard API capabilities", () => {
    render(<CapabilityProfileList profiles={[
      {
        protocol: "openai_responses",
        capability_layer: "driver",
        vision: true,
        verification_state: "declared",
      },
      {
        protocol: "openai_responses",
        capability_layer: "client_host",
        file_input: true,
        verification_state: "declared",
      },
    ]} />);

    expect(screen.getByRole("heading", { name: "主要能力" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "在工具中可用" })).toBeInTheDocument();
    expect(screen.getByText("Codex / Agent 可读取本地文件")).toBeInTheDocument();
    expect(screen.queryByText(/^文件输入$/)).not.toBeInTheDocument();
  });
});
