import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { changeAppLanguage, tr } from "../i18n";
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
    onCopyImageCommand: vi.fn(),
    onOpenMediaSkillSettings: vi.fn(),
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
    const { onCopy, onOpenRoot, onClose } = props;
    const { container } = render(<ConnectionGuideModal {...props} />);

    const protocols = within(screen.getByRole("region", { name: "协议与接入地址" }));
    expect(screen.queryByText("兼容接入")).not.toBeInTheDocument();
    for (const name of ["OpenAI · Responses / Chat Completions", "Anthropic · Messages", "Gemini · generateContent / streamGenerateContent"]) {
      expect(protocols.getByRole("region", { name })).toBeInTheDocument();
    }

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

    expect(screen.getByText("sk-local-test")).toBeInTheDocument();

    const closeButton = container.querySelector<HTMLButtonElement>(".modal-close-button");
    expect(closeButton).not.toBeNull();
    await user.click(closeButton!);
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("loads once on expansion, reuses late results, and copies the same public IDs it displays", async () => {
    const user = userEvent.setup();
    const props = { ...guideProps(), availableModels: [
      "openai/GPT-5.6", "gpt-5.6", "gateway/anthropic/Claude-5[1M]", "claude-5",
      "vendor/Model:free", "incomplete/",
    ] };
    const { rerender } = render(<ConnectionGuideModal {...props} modelStatus="idle" />);
    const steps = screen.getByRole("heading", { name: "三步接入" }).closest("section")!;
    const toggle = within(within(steps).getAllByRole("listitem")[2]).getByRole("button", { name: "查看模型 ID" });
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(props.onRefreshModels).not.toHaveBeenCalled();
    expect(screen.queryByText(tr("access.guide.modelsLoading"))).not.toBeInTheDocument();

    await user.click(toggle);
    expect(await screen.findByText(tr("access.guide.modelsLoading"))).toBeVisible();
    expect(props.onRefreshModels).toHaveBeenCalledOnce();
    await user.click(toggle);
    rerender(<ConnectionGuideModal {...props} />);
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByText("gpt-5.6")).not.toBeInTheDocument();

    await user.click(toggle);
    expect(props.onRefreshModels).toHaveBeenCalledOnce();
    expect(screen.getByText(tr("access.guide.modelsAvailable", { count: 3 }))).toBeVisible();
    await user.click(await screen.findByRole("button", { name: "复制模型 gpt-5.6" }));
    expect(props.onCopy).toHaveBeenLastCalledWith("guide-model-id", "gpt-5.6", "gpt-5.6");
    await user.click(screen.getByRole("button", { name: "复制全部" }));
    expect(props.onCopy).toHaveBeenLastCalledWith("guide-models", "可填写的模型 ID", "gpt-5.6\nclaude-5\nmodel:free");
    await user.click(screen.getByRole("button", { name: "刷新" }));
    expect(props.onRefreshModels).toHaveBeenCalledTimes(2);
    expect(props.onRefreshModels).toHaveBeenLastCalledWith({ force: true });
  });

  it("gives media its own section and copies each workflow without including the local key", async () => {
    const user = userEvent.setup();
    const props = { ...guideProps(), openaiBaseUrl: "http://[::1]:43127/v1" };
    const { rerender } = render(<ConnectionGuideModal {...props} />);
    const media = screen.getByRole("group", { name: "媒体生成" });
    expect(media.parentElement).toBe(screen.getByRole("region", { name: "协议与接入地址" }).parentElement);
    expect(media.previousElementSibling).toContainElement(screen.getByRole("heading", { name: "三步接入" }));
    expect(media.nextElementSibling).toBeNull();
    expect(media).not.toHaveAttribute("open");
    await user.click(within(media).getByText("媒体生成"));
    expect(within(media).getByText(/只查目录，不生成/)).toBeVisible();
    const manualSetup = within(media).getByText(tr("access.guide.mediaApiTitle"));
    expect(manualSetup.closest("details")).not.toHaveAttribute("open");
    await user.click(manualSetup);
    for (const route of ["POST /images/generations", "POST /images/edits", "POST /videos", "GET /videos/{id}", "POST /audio/speech"]) {
      expect(within(media).getByText(route)).toBeVisible();
    }

    await user.click(within(media).getByRole("button", { name: "复制图片命令用法" }));
    expect(props.onCopyImageCommand).toHaveBeenCalledOnce();
    expect(props.onCopy).not.toHaveBeenCalled();

    const workflows = [
      { field: "guide-image-usage", label: "复制图片 API 用法", paths: ["/models", "/images/generations", "/images/edits"], content: ["image_operations", "multipart/form-data", "b64_json", "data[0].url"] },
      { field: "guide-video-usage", label: "复制视频生成用法", paths: ["/videos", "/videos/{id}", "/videos/{id}/content"], content: ["<PROMPT>", "request_id", "video.url"] },
      { field: "guide-speech-usage", label: "复制语音合成用法", paths: ["/audio/speech"], content: ["<VOICE>", "<TEXT>", "speech.mp3", "speech.audio.delta", "speech.audio.done"] },
    ];
    for (const { field, label, paths, content } of workflows) {
      await user.click(within(media).getByRole("button", { name: label }));
      expect(props.onCopy).toHaveBeenLastCalledWith(field, label, expect.any(String));
      const instructions: string = props.onCopy.mock.lastCall![2];
      for (const path of paths) expect(instructions).toContain(props.openaiBaseUrl + path);
      for (const term of ["Bearer <LOCAL_API_KEY>", "<MODEL>", ...content]) expect(instructions).toContain(term);
      expect(instructions).not.toContain(props.apiKey);
    }
    rerender(<ConnectionGuideModal {...props} copiedField="guide-video-usage" />);
    expect(within(media).getByRole("button", { name: "已复制" })).toBeVisible();
    expect(within(media).getByRole("button", { name: "复制语音合成用法" })).toBeVisible();
    expect(within(media).getByRole("button", { name: "复制图片命令用法" })).toBeVisible();
  });

  it.each(["zh-CN", "en-US"] as const)("copies exactly the visible media example in %s", async (language) => {
    await changeAppLanguage(language);
    const user = userEvent.setup();
    const props = guideProps();
    const { rerender } = render(<ConnectionGuideModal {...props} />);
    const media = within(screen.getByRole("group", { name: tr("access.guide.mediaTitle") }));
    await user.click(media.getByText(tr("access.guide.mediaTitle")));
    expect(media.getByRole("heading", { name: tr("access.guide.mediaChatTitle") })).toBeVisible();
    const settingsLabel = tr("access.guide.mediaSkillSettings", { setting: tr("access.toolImageSkill") });
    await user.click(media.getByRole("button", { name: settingsLabel }));
    expect(props.onOpenMediaSkillSettings).toHaveBeenCalledOnce();
    expect(props.onCopy).not.toHaveBeenCalled();

    for (const kind of ["discover", "generate", "edit", "video", "speech", "music"] as const) {
      const label = tr(`access.guide.promptLabels.${kind}`);
      const prompt = tr(`access.guide.prompts.${kind}`);
      const field = `guide-prompt-${kind}` as const;
      const row = media.getByText(prompt).closest("dd")!;
      const button = within(row).getByRole("button", { name: tr("access.guide.copyPrompt", { label }) });
      await user.click(button);
      expect(props.onCopy).toHaveBeenLastCalledWith(field, label, prompt);
      expect(prompt).not.toContain(props.apiKey);
      rerender(<ConnectionGuideModal {...props} copiedField={field} />);
      expect(media.getByRole("button", { name: tr("common.copied") })).toBe(button);
    }
    expect(props.onCopy).toHaveBeenCalledTimes(6);
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
    const media = screen.getByRole("group", { name: "Media generation" });
    await user.click(within(media).getByText("Media generation"));
    await user.click(within(media).getByText(tr("access.guide.mediaApiTitle")));
    expect(within(media).getByRole("button", { name: "Copy video generation instructions" })).toBeVisible();
    await user.click(within(media).getByRole("button", { name: "Copy speech synthesis instructions" }));
    expect(props.onCopy).toHaveBeenLastCalledWith(
      "guide-speech-usage", "Copy speech synthesis instructions", expect.stringContaining("http://[::1]:43127/v1/audio/speech"),
    );
  });
});
