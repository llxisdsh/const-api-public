import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { changeAppLanguage } from "../i18n";
import { ConnectionGuideModal } from "./ConnectionGuideModal";

function guideProps() {
  return {
    rootUrl: "http://127.0.0.1:38787/",
    apiKey: "sk-local-test",
    openaiBaseUrl: "http://127.0.0.1:38787/v1",
    openaiChatUrl: "http://127.0.0.1:38787/v1/chat/completions",
    anthropicBaseUrl: "http://127.0.0.1:38787/anthropic",
    geminiBaseUrl: "http://127.0.0.1:38787/gemini",
    availableModels: ["gpt-5.6", "claude-5"],
    modelStatus: "ready" as const,
    copiedField: null,
    onCopy: vi.fn(),
    onRefreshModels: vi.fn(),
    onOpenRoot: vi.fn(),
    onClose: vi.fn(),
  };
}

afterEach(async () => {
  await changeAppLanguage("zh-CN");
});

describe("ConnectionGuideModal", () => {
  it("groups base, request and discovery URLs by protocol in one connection table", async () => {
    const user = userEvent.setup();
    const props = guideProps();
    const { onCopy, onRefreshModels, onOpenRoot, onClose } = props;
    const { container } = render(<ConnectionGuideModal {...props} />);

    expect(container.querySelectorAll(".guide-surface-section")).toHaveLength(1);
    expect(screen.queryByText("兼容接入")).not.toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "OpenAI · Responses / Chat Completions" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Anthropic · Messages" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Gemini · generateContent / streamGenerateContent" })).toBeInTheDocument();

    await user.click(screen.getByRole("dialog"));
    expect(onClose).not.toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: /http:\/\/127\.0\.0\.1:38787\// }));
    expect(onOpenRoot).toHaveBeenCalledWith("http://127.0.0.1:38787/");

    const addresses = [
      ["guide-openai-url", "OpenAI Base URL", "/v1"],
      ["guide-openai-responses-url", "OpenAI Responses URL", "/v1/responses"],
      ["guide-openai-chat-url", "OpenAI Chat Completions URL", "/v1/chat/completions"],
      ["guide-openai-models-url", "OpenAI 模型发现 URL", "/v1/models"],
      ["guide-anthropic-url", "Anthropic Base URL", "/anthropic"],
      ["guide-anthropic-messages-url", "Anthropic Messages URL", "/anthropic/v1/messages"],
      ["guide-anthropic-models-url", "Anthropic 模型发现 URL", "/anthropic/v1/models"],
      ["guide-gemini-url", "Gemini Base URL", "/gemini"],
      ["guide-gemini-generate-url", "Gemini 请求 URL", "/gemini/v1beta/models/{model}:generateContent"],
      ["guide-gemini-stream-url", "Gemini 流式请求 URL", "/gemini/v1beta/models/{model}:streamGenerateContent?alt=sse"],
      ["guide-gemini-models-url", "Gemini 模型发现 URL", "/gemini/v1beta/models"],
    ];
    for (const [field, label, path] of addresses) {
      await user.click(screen.getByRole("button", { name: `复制 ${label}` }));
      expect(onCopy).toHaveBeenLastCalledWith(field, label, `http://127.0.0.1:38787${path}`);
    }
    expect(screen.getByText("请求地址中的 {model} 需替换为实际模型 ID。")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "复制本地 API Key" }));
    expect(onCopy).toHaveBeenCalledWith("api-key", "本地 API Key", "sk-local-test");

    await user.click(screen.getByRole("button", { name: "复制模型 gpt-5.6" }));
    expect(onCopy).toHaveBeenCalledWith("guide-model-id", "gpt-5.6", "gpt-5.6");

    await user.click(screen.getByRole("button", { name: "刷新" }));
    expect(onRefreshModels).toHaveBeenCalledOnce();

    await user.click(screen.getByRole("button", { name: "复制全部" }));
    expect(onCopy).toHaveBeenLastCalledWith("guide-models", "可填写的模型 ID", "gpt-5.6\nclaude-5");
    expect(screen.getByText("sk-local-test")).toBeInTheDocument();

    const closeButton = container.querySelector<HTMLButtonElement>(".modal-close-button");
    expect(closeButton).not.toBeNull();
    await user.click(closeButton!);
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("keeps discovery copy labels localized and derives URLs from the current base addresses", async () => {
    await changeAppLanguage("en-US");
    const user = userEvent.setup();
    const props = guideProps();
    const { rerender } = render(<ConnectionGuideModal {...props} />);
    expect(screen.getByRole("heading", { name: "Protocols and connection URLs" })).toBeInTheDocument();
    for (const provider of ["OpenAI", "Anthropic", "Gemini"]) {
      expect(screen.getByRole("button", { name: `Copy ${provider} Model discovery URL` })).toBeInTheDocument();
    }

    rerender(<ConnectionGuideModal {...props}
      openaiBaseUrl="http://[::1]:43127/v1"
      openaiChatUrl="http://[::1]:43127/v1/chat/completions"
      anthropicBaseUrl="http://[::1]:43127/anthropic"
      geminiBaseUrl="http://[::1]:43127/gemini"
      copiedField="guide-anthropic-models-url"
    />);
    await user.click(screen.getByRole("button", { name: "Copy Gemini Model discovery URL" }));
    expect(props.onCopy).toHaveBeenLastCalledWith(
      "guide-gemini-models-url", "Gemini Model discovery URL", "http://[::1]:43127/gemini/v1beta/models",
    );
    await user.click(screen.getByRole("button", { name: "Copy OpenAI Responses URL" }));
    expect(props.onCopy).toHaveBeenLastCalledWith(
      "guide-openai-responses-url", "OpenAI Responses URL", "http://[::1]:43127/v1/responses",
    );
    expect(screen.getByRole("button", { name: "Copied" })).toHaveClass("copied");
    expect(screen.getByText("http://[::1]:43127/anthropic/v1/models")).toBeInTheDocument();
  });
});
