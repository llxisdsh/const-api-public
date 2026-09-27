import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { ConnectionGuideModal } from "./ConnectionGuideModal";

describe("ConnectionGuideModal", () => {
  it("separates standard base URLs from full endpoints and manual model IDs", async () => {
    const user = userEvent.setup();
    const onCopy = vi.fn();
    const onRefreshModels = vi.fn();
    const onOpenRoot = vi.fn();
    const onClose = vi.fn();

    const { container } = render(
      <ConnectionGuideModal
        rootUrl="http://127.0.0.1:38787/"
        apiKey="sk-local-test"
        openaiBaseUrl="http://127.0.0.1:38787/v1"
        openaiChatUrl="http://127.0.0.1:38787/v1/chat/completions"
        anthropicBaseUrl="http://127.0.0.1:38787/anthropic"
        geminiBaseUrl="http://127.0.0.1:38787/gemini"
        availableModels={["gpt-5.6", "claude-5"]}
        modelStatus="ready"
        copiedField={null}
        onCopy={onCopy}
        onRefreshModels={onRefreshModels}
        onOpenRoot={onOpenRoot}
        onClose={onClose}
      />,
    );

    await user.click(screen.getByRole("dialog"));
    expect(onClose).not.toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: /http:\/\/127\.0\.0\.1:38787\// }));
    expect(onOpenRoot).toHaveBeenCalledWith("http://127.0.0.1:38787/");

    await user.click(screen.getByRole("button", { name: "复制 Anthropic Base URL" }));
    expect(onCopy).toHaveBeenCalledWith(
      "guide-anthropic-url",
      "Anthropic Base URL",
      "http://127.0.0.1:38787/anthropic",
    );

    await user.click(screen.getByRole("button", { name: "复制本地 API Key" }));
    expect(onCopy).toHaveBeenCalledWith("api-key", "本地 API Key", "sk-local-test");

    await user.click(screen.getByRole("button", { name: "复制 OpenAI Chat Completions URL" }));
    expect(onCopy).toHaveBeenCalledWith(
      "guide-openai-chat-url",
      "OpenAI Chat Completions URL",
      "http://127.0.0.1:38787/v1/chat/completions",
    );

    await user.click(screen.getByRole("button", { name: "复制模型 gpt-5.6" }));
    expect(onCopy).toHaveBeenCalledWith("guide-model-id", "gpt-5.6", "gpt-5.6");

    await user.click(screen.getByRole("button", { name: "刷新" }));
    expect(onRefreshModels).toHaveBeenCalledOnce();

    expect(screen.queryByText("/anthropic/v1/messages")).not.toBeInTheDocument();
    expect(screen.getByText("sk-local-test")).toBeInTheDocument();

    const closeButton = container.querySelector<HTMLButtonElement>(".modal-close-button");
    expect(closeButton).not.toBeNull();
    await user.click(closeButton!);
    expect(onClose).toHaveBeenCalledOnce();
  });
});
