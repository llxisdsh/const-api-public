// @vitest-environment jsdom

import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { Circle } from "lucide-react";
import { describe, expect, test, vi } from "vitest";
import { ToolDockMenuProvider } from "../ToolDockMenu";
import { installWebviewShortcutGuard } from "../webviewShortcutGuard";
import {
  BlockingConfirmDialog,
  ClaudeModelSettingsPanel,
  CompactChoiceMenu,
  HiddenToolMenu,
  ModelInput,
  QuickCard,
  ToolRemoveDialog,
} from "./AppControls";

describe("ClaudeModelSettingsPanel", () => {
  test("keeps model choices as a draft for the shared Configure action", () => {
    render(
      <ClaudeModelSettingsPanel
        settings={{
          main: "gpt-5.6-sol",
          opus: "gpt-5.6-sol",
          sonnet: "gpt-5.6-sol",
          haiku: "gpt-5.6-sol",
        }}
        models={["gpt-5.6-sol"]}
        loading={false}
        changed
        disabled={false}
        onChange={vi.fn()}
      />,
    );

    expect(screen.getByText("待配置")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "应用并重启 Claude Code" })).toBeNull();
  });
});

describe("ModelInput", () => {
  test("hides context hints in labels without changing wire IDs or priced variants", () => {
    const onChange = vi.fn();
    const models = [
      "gpt-5.6-luna",
      "qwen/qwen3.8-27b",
      "anthropic/vendor/model:free",
      "anthropic/claude-sonnet-5[1m]",
    ];
    render(<ModelInput value={models[1]} models={models} onChange={onChange} />);

    const select = screen.getByRole("combobox");
    expect(select).toHaveDisplayValue("qwen3.8-27b");
    expect(select).toHaveValue("qwen/qwen3.8-27b");
    expect(screen.getAllByRole("option").map((option) => option.textContent)).toEqual([
      "claude-sonnet-5", "gpt-5.6-luna", "model:free", "qwen3.8-27b",
    ]);
    expect(screen.getAllByRole("option").map((option) => (option as HTMLOptionElement).value)).toEqual([models[3], models[0], models[2], models[1]]);

    fireEvent.change(select, { target: { value: models[2] } });
    expect(onChange).toHaveBeenCalledWith("anthropic/vendor/model:free");
  });

  test("keeps the first short-name source and updates a colliding saved selection", () => {
    const models = ["model", "vendor-a/model", "gateway/vendor-b/Model", "vendor-a/model:free"];
    const onChange = vi.fn();
    render(<ModelInput value={models[1]} models={models} onChange={onChange} />);

    expect(screen.getAllByRole("option").map((option) => option.textContent)).toEqual([
      "model", "model:free",
    ]);
    expect(screen.getByRole("combobox")).toHaveValue("model");
    expect(onChange).toHaveBeenCalledWith("model");
    fireEvent.change(screen.getByRole("combobox"), { target: { value: models[3] } });
    expect(onChange).toHaveBeenCalledWith("vendor-a/model:free");
  });

  test("uses the first refreshed full name for a colliding missing selection", () => {
    const onChange = vi.fn();
    render(<ModelInput value="old/model" models={["new/model"]} onChange={onChange} />);

    expect(screen.getByRole("combobox")).toHaveValue("new/model");
    expect(screen.getByRole("combobox")).toHaveDisplayValue("model");
    expect(screen.getAllByRole("option").map((option) => option.textContent)).toEqual([
      "model",
    ]);
    expect(onChange).toHaveBeenCalledWith("new/model");
  });

  test("keeps manual entry unchanged when no model list is available", () => {
    const onChange = vi.fn();
    render(<ModelInput value="vendor/model" models={[]} onChange={onChange} />);

    const input = screen.getByRole("textbox");
    expect(input).toHaveValue("vendor/model");
    fireEvent.change(input, { target: { value: "other/model" } });
    expect(onChange).toHaveBeenCalledWith("other/model");
  });

  test("keeps the native model menu on the shared select styling path", () => {
    const onChange = vi.fn();
    const { container } = render(
      <ModelInput
        value="gpt-5.6-sol"
        models={["gpt-5.6-sol", "gpt-5.6-terra"]}
        onChange={onChange}
      />,
    );

    const select = screen.getByRole("combobox");
    expect(select).toHaveClass("model-input-select");
    expect(select.closest(".model-input-select-shell")).toBeNull();
    expect(container.querySelector(".model-input-select-shell")).toBeNull();

    fireEvent.change(select, { target: { value: "gpt-5.6-terra" } });
    expect(onChange).toHaveBeenCalledWith("gpt-5.6-terra");
  });
});

describe("BlockingConfirmDialog", () => {
  test("keeps backdrop clicks inert and maps Escape to the cancel path", () => {
    const onResolve = vi.fn();
    render(
      <BlockingConfirmDialog
        dialog={{ title: "Hermes 正在运行", message: "请确认下一步。" }}
        onResolve={onResolve}
      />,
    );

    const dialog = screen.getByRole("dialog", { name: "Hermes 正在运行" });
    expect(dialog).toHaveAttribute("data-backdrop-dismissible", "false");
    fireEvent.click(dialog);
    expect(onResolve).not.toHaveBeenCalled();

    fireEvent.keyDown(document, { key: "Escape" });
    expect(onResolve).toHaveBeenCalledWith(false);
  });

  test("renders notice results with one acknowledgement action", () => {
    const onResolve = vi.fn();
    render(
      <BlockingConfirmDialog
        dialog={{
          title: "取消配置完成",
          message: "已恢复配置。\n\n请重新启动工具。",
          confirmText: "我知道了",
          mode: "notice",
        }}
        onResolve={onResolve}
      />,
    );

    expect(screen.queryByRole("button", { name: "取消" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "我知道了" }));
    expect(onResolve).toHaveBeenCalledWith(true);
  });
});

describe("ToolRemoveDialog", () => {
  test("confirms the pre-CONST API restore mode by default", () => {
    const onResolve = vi.fn();
    render(
      <ToolRemoveDialog
        dialog={{
          tool: "codex",
          toolTitle: "Codex",
          nativeOption: {
            title: "切换到 OpenAI / ChatGPT 原生接入",
            description: "保留有效的官方登录。",
          },
        }}
        onResolve={onResolve}
      />,
    );

    const restore = screen.getByRole("radio", { name: /恢复 CONST API 接管前状态/ });
    const native = screen.getByRole("radio", { name: /OpenAI \/ ChatGPT 原生接入/ });
    expect(restore).toBeChecked();
    expect(native).not.toBeChecked();
    expect(screen.getByText("默认")).toHaveClass("tool-remove-option-badge");
    expect(screen.queryByText(
      "选择取消后要恢复到哪种接入状态。默认选项保持当前逻辑。",
    )).not.toBeInTheDocument();
    expect(screen.getByText(
      "继续后，如发现 Codex 仍在运行，将自动关闭相关进程后再取消配置。",
    )).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "取消配置" }));
    expect(onResolve).toHaveBeenCalledWith("restore_pre_const");
  });

  test("allows choosing a supported tool's native route", () => {
    const onResolve = vi.fn();
    render(
      <ToolRemoveDialog
        dialog={{
          tool: "codex",
          toolTitle: "Codex",
          nativeOption: {
            title: "切换到 OpenAI / ChatGPT 原生接入",
            description: "保留有效的官方登录。",
          },
        }}
        onResolve={onResolve}
      />,
    );

    const native = screen.getByRole("radio", { name: /OpenAI \/ ChatGPT 原生接入/ });
    fireEvent.click(native);
    fireEvent.click(screen.getByRole("button", { name: "取消配置" }));
    expect(onResolve).toHaveBeenCalledWith("native_route");
  });

  test("shows only the restore choice for tools without a canonical native route", () => {
    const onResolve = vi.fn();
    render(
      <ToolRemoveDialog
        dialog={{ tool: "vscode", toolTitle: "VS Code" }}
        onResolve={onResolve}
      />,
    );

    expect(screen.getAllByRole("radio")).toHaveLength(1);
    expect(screen.queryByText(
      "此工具没有唯一的原生接入目标，将按现有逻辑恢复 CONST API 接管前保存的状态。",
    )).not.toBeInTheDocument();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(onResolve).toHaveBeenCalledWith("cancel");
  });
});

describe("QuickCard", () => {
  test("hides cancel configuration when the caller provides no removal action", () => {
    const onConfigure = vi.fn();
    render(<ToolDockMenuProvider><QuickCard
      id="test-tool" title="Test Tool" icon={null} fallback={Circle}
      tone="#111827" actionLabel="配置" statusText="需写入配置"
      onAction={vi.fn()} onConfigure={onConfigure} onLocate={vi.fn()}
    /></ToolDockMenuProvider>);
    fireEvent.contextMenu(screen.getByRole("button", { name: "Test Tool" }));
    expect(screen.queryByRole("button", { name: "取消配置" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "配置" }));
    expect(onConfigure).toHaveBeenCalledOnce();
  });

  test("shows the green marker only for an already configured tool", () => {
    render(
      <ToolDockMenuProvider>
        <>
          <QuickCard
            id="configured-tool"
            title="Configured Tool"
            icon={null}
            fallback={Circle}
            tone="#111827"
            configured
            actionLabel="Configure"
            onAction={vi.fn()}
          />
          <QuickCard
            id="unconfigured-tool"
            title="Unconfigured Tool"
            icon={null}
            fallback={Circle}
            tone="#111827"
            actionLabel="Configure"
            onAction={vi.fn()}
          />
        </>
      </ToolDockMenuProvider>,
    );

    const configuredCard = screen.getByRole("button", { name: "Configured Tool" }).closest(".tool-dock");
    const unconfiguredCard = screen.getByRole("button", { name: "Unconfigured Tool" }).closest(".tool-dock");
    expect(configuredCard?.querySelector(".tool-dock-configured-dot")).toBeInTheDocument();
    expect(configuredCard?.querySelector(".tool-dock-hover-name-label")).toHaveTextContent("Configured Tool");
    expect(unconfiguredCard?.querySelector(".tool-dock-configured-dot")).toBeNull();
    expect(unconfiguredCard?.querySelector(".tool-dock-hover-name-label")).toHaveTextContent("Unconfigured Tool");
  });

  test("does not claim a colored status before the first config check completes", () => {
    const { container } = render(
      <ToolDockMenuProvider>
        <QuickCard
          id="codex-checking"
          title="Codex"
          icon={null}
          fallback={Circle}
          tone="#111827"
          statusText="检查中"
          statusTone="unknown"
          actionLabel="配置"
          onAction={vi.fn()}
          onConfigure={vi.fn()}
          onRemoveConfig={vi.fn()}
          onLocate={vi.fn()}
        />
      </ToolDockMenuProvider>,
    );

    expect(screen.getByRole("button", { name: "Codex" })).toBeInTheDocument();
    expect(container.querySelector(".tool-dock-status")).toBeNull();
  });

  test("keeps the app-owned tool menu under the packaged WebView guard", () => {
    const uninstallGuard = installWebviewShortcutGuard(window);
    try {
      render(
        <ToolDockMenuProvider>
          <QuickCard
            id="guarded-tool"
            title="Guarded Tool"
            icon={null}
            fallback={Circle}
            tone="#111827"
            statusText="Needs setup"
            statusTone="missing"
            actionLabel="Configure"
            onAction={vi.fn()}
            onConfigure={vi.fn()}
          />
        </ToolDockMenuProvider>,
      );

      const tool = screen.getByRole("button", { name: "Guarded Tool" });
      const nativeMenuAllowed = fireEvent.contextMenu(tool, { button: 2 });

      expect(nativeMenuAllowed).toBe(false);
      expect(screen.getByRole("button", { name: "Configure" })).toBeInTheDocument();
    } finally {
      uninstallGuard();
    }
  });

  test("launches from a tool logo click and opens its menu from the context gesture", () => {
    const onAction = vi.fn();
    const onConfigure = vi.fn();
    const onHide = vi.fn();
    const { container } = render(
      <ToolDockMenuProvider>
        <QuickCard
          id="codex"
          title="Codex"
          description="OpenAI Codex，兼容新版 ChatGPT 与旧版 Codex 桌面应用"
          icon={null}
          fallback={Circle}
          tone="#111827"
          statusText="需写入配置"
          statusTone="missing"
          actionLabel="配置"
          onAction={onAction}
          onConfigure={onConfigure}
          onRemoveConfig={vi.fn()}
          onLocate={vi.fn()}
          onHide={onHide}
        />
      </ToolDockMenuProvider>,
    );

    const logo = screen.getByRole("button", { name: "Codex" });
    const reservedStatusArea = container.querySelector(".tool-dock-status-text");
    expect(reservedStatusArea).toBeInTheDocument();
    expect(reservedStatusArea?.querySelector(".tool-dock-hover-name")).toHaveTextContent("Codex");
    expect(reservedStatusArea).toHaveAttribute("aria-hidden", "true");
    expect(screen.queryByText("需写入配置")).toBeNull();
    expect(document.querySelector(".tool-dock-status")).toBeNull();
    expect(logo).not.toHaveAttribute("title");
    fireEvent.pointerEnter(reservedStatusArea as Element);
    expect(container.querySelector(".quick-card-menu-panel")).toBeNull();
    fireEvent.click(logo);

    expect(onAction).toHaveBeenCalledTimes(1);
    expect(container.querySelector(".quick-card-menu-panel")).toBeNull();
    fireEvent.contextMenu(logo, { button: 2 });

    expect(onAction).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("button", { name: "启动" })).toBeNull();
    expect(screen.getAllByText("Codex").find((element) => element.tagName === "STRONG"))
      .toHaveAttribute("title", "OpenAI Codex，兼容新版 ChatGPT 与旧版 Codex 桌面应用");
    expect(screen.queryByText("OpenAI 桌面应用")).toBeNull();
    const configure = screen.getByRole("button", { name: "配置" });
    expect(configure.querySelector("svg")).toBeNull();
    expect(screen.queryByRole("button", { name: "仅配置" })).not.toBeInTheDocument();
    expect(onConfigure).not.toHaveBeenCalled();
    const configureAction = container.querySelector(".primary-menu-action");
    expect(configureAction).toBeInTheDocument();
    expect(configureAction?.querySelector(".tool-dock-menu-status-dot")).toBeNull();
    expect(container.querySelector(".tool-dock-menu-title-status.missing")).toBeInTheDocument();
    fireEvent.click(configureAction as Element);
    expect(onConfigure).toHaveBeenCalledTimes(1);
    expect(onAction).toHaveBeenCalledTimes(1);

    fireEvent.contextMenu(logo, { button: 2 });
    fireEvent.click(screen.getByRole("button", { name: "不显示" }));
    expect(onHide).toHaveBeenCalledTimes(1);
  });

  test("keeps the active tool or add menu stable while the pointer crosses other icons", () => {
    render(
      <ToolDockMenuProvider>
        {["Codex", "Claude"].map((title) => (
          <QuickCard
            key={title}
            id={title}
            title={title}
            icon={null}
            fallback={Circle}
            tone="#111827"
            actionLabel="Configure"
            onAction={vi.fn()}
            panels={[{ id: "details", label: "More", content: <span>{title} settings</span> }]}
          />
        ))}
        <HiddenToolMenu cards={[]} onShow={vi.fn()} onResetOrder={vi.fn()} />
      </ToolDockMenuProvider>,
    );

    const codex = screen.getByRole("button", { name: "Codex" });
    const claude = screen.getByRole("button", { name: "Claude" });
    const add = screen.getByRole("button", { name: "显示更多工具" });
    fireEvent.contextMenu(claude, { button: 2 });
    fireEvent.click(screen.getByRole("button", { name: "Claude More" }));

    for (const otherIcon of [codex, add]) {
      fireEvent.pointerEnter(otherIcon);
      fireEvent.pointerLeave(otherIcon);
      expect(claude).toHaveAttribute("aria-expanded", "true");
      expect(otherIcon).toHaveAttribute("aria-expanded", "false");
      expect(screen.getByText("Claude settings")).toBeInTheDocument();
    }

    fireEvent.contextMenu(codex, { button: 2 });
    expect(codex).toHaveAttribute("aria-expanded", "true");
    expect(claude).toHaveAttribute("aria-expanded", "false");
    fireEvent.click(add);
    fireEvent.pointerEnter(codex);
    expect(add).toHaveAttribute("aria-expanded", "true");
    expect(codex).toHaveAttribute("aria-expanded", "false");

    fireEvent.keyDown(document, { key: "Escape" });
    expect(add).toHaveAttribute("aria-expanded", "false");
    fireEvent.contextMenu(claude, { button: 2 });
    fireEvent.pointerDown(document.body);
    expect(claude).toHaveAttribute("aria-expanded", "false");
  });

  test("opens the tool menu after a long press without launching", () => {
    vi.useFakeTimers();
    try {
      const onAction = vi.fn();
      const { container } = render(
        <ToolDockMenuProvider>
          <QuickCard
            id="hermes"
            title="Hermes"
            icon={null}
            fallback={Circle}
            tone="#111827"
            statusText="Ready"
            statusTone="ready"
            actionLabel="Configure"
            onAction={onAction}
            onConfigure={vi.fn()}
            onRemoveConfig={vi.fn()}
            onLocate={vi.fn()}
          />
        </ToolDockMenuProvider>,
      );

      const logo = screen.getByRole("button", { name: "Hermes" });
      fireEvent.pointerDown(logo, { button: 0, pointerId: 1, clientX: 12, clientY: 12 });
      act(() => vi.advanceTimersByTime(499));
      expect(container.querySelector(".quick-card-menu-panel")).toBeNull();

      act(() => vi.advanceTimersByTime(1));
      expect(container.querySelector(".quick-card-menu-panel")).toBeInTheDocument();
      fireEvent.pointerUp(logo, { button: 0, pointerId: 1, clientX: 12, clientY: 12 });
      fireEvent.click(logo);
      expect(onAction).not.toHaveBeenCalled();

      fireEvent.pointerDown(logo, { button: 0, pointerId: 2, clientX: 12, clientY: 12 });
      fireEvent.pointerUp(logo, { button: 0, pointerId: 2, clientX: 12, clientY: 12 });
      fireEvent.click(logo);
      expect(onAction).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  test("keeps the parent tool menu open after a nested choice closes", () => {
    vi.useFakeTimers();
    try {
      const { container } = render(
        <ToolDockMenuProvider>
          <QuickCard
            id="vscode"
            title="VS Code"
            icon={null}
            fallback={Circle}
            tone="#111827"
            statusText="已配置"
            statusTone="ready"
            actionLabel="配置"
            onAction={vi.fn()}
            onConfigure={vi.fn()}
            onRemoveConfig={vi.fn()}
            onLocate={vi.fn()}
            panels={[{
              id: "tool-info",
              label: "更多",
              content: (
                <CompactChoiceMenu
                  ariaLabel="VS Code 接入协议"
                  value="responses"
                  options={[
                    { value: "responses", label: "Responses" },
                    { value: "chat", label: "Chat" },
                  ]}
                  onChange={vi.fn()}
                />
              ),
            }]}
          />
        </ToolDockMenuProvider>,
      );

      const dock = container.querySelector(".tool-dock") as Element;
      fireEvent.contextMenu(screen.getByRole("button", { name: "VS Code" }), { button: 2 });
      fireEvent.click(screen.getByRole("button", { name: "VS Code 更多" }));

      expect(container.querySelector(".tool-dock-menu-title-status.ready")).toBeInTheDocument();
      expect(container.querySelector(".primary-menu-action .tool-dock-menu-status-dot")).toBeNull();
      expect(container.querySelector(".tool-dock-context-status")).toBeNull();
      expect(container.querySelector(".tool-dock-menu-actions > .tool-dock-context-toggle")).toBeInTheDocument();
      expect(container.querySelector(".tool-dock-context-header")).toBeNull();
      expect(container.querySelector(".quick-card-status")).toBeNull();

      fireEvent.click(screen.getByRole("button", { name: "VS Code 接入协议" }));

      const chatOption = screen.getByRole("option", { name: "Chat" });
      fireEvent.pointerDown(chatOption);
      fireEvent.click(chatOption);
      fireEvent.pointerLeave(dock);
      vi.advanceTimersByTime(400);

      expect(screen.queryByRole("listbox")).toBeNull();
      expect(screen.getByRole("button", { name: "配置" })).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: "启动" })).toBeNull();

      fireEvent.pointerDown(document.body);
      expect(screen.queryByRole("button", { name: "配置" })).toBeNull();

      fireEvent.contextMenu(screen.getByRole("button", { name: "VS Code" }), { button: 2 });
      expect(container.querySelector(".tool-dock-context")).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  test("clamps regular and expanded menus with the shared content-boundary placement", async () => {
    const originalInnerWidth = Object.getOwnPropertyDescriptor(window, "innerWidth");
    Object.defineProperty(window, "innerWidth", {
      configurable: true,
      value: 502,
    });
    const rect = (left: number, right: number): DOMRect => ({
      bottom: 80,
      height: 80,
      left,
      right,
      top: 0,
      width: right - left,
      x: left,
      y: 0,
      toJSON: () => ({}),
    });

    try {
      const { container } = render(
        <main>
          <ToolDockMenuProvider>
            <QuickCard
              id="raven-placement"
              title="Raven"
              description="Raven agent"
              icon={null}
              fallback={Circle}
              tone="#0153e5"
              actionLabel="配置"
              onAction={vi.fn()}
              panels={[{
                id: "details",
                label: "更多",
                content: <div>Raven details</div>,
              }]}
            />
          </ToolDockMenuProvider>
        </main>,
      );

      const main = container.querySelector("main") as HTMLElement;
      const dock = container.querySelector(".tool-dock") as HTMLDivElement;
      const toolButton = screen.getByRole("button", { name: "Raven" });
      vi.spyOn(main, "getBoundingClientRect").mockReturnValue(rect(150, 502));
      vi.spyOn(dock, "getBoundingClientRect").mockReturnValue(rect(372, 436));
      vi.spyOn(toolButton, "getBoundingClientRect").mockReturnValue(rect(380, 428));

      fireEvent.contextMenu(toolButton, { button: 2 });
      const menu = container.querySelector("#raven-placement-tool-menu") as HTMLDivElement;
      expect(menu).toHaveStyle({ left: "-54px", width: "172px" });

      fireEvent.click(screen.getByRole("button", { name: "Raven 更多" }));
      await waitFor(() => expect(menu).toHaveStyle({ left: "-182px", width: "300px" }));
    } finally {
      if (originalInnerWidth) {
        Object.defineProperty(window, "innerWidth", originalInnerWidth);
      }
    }
  });

  test("restores a hidden tool from the dock-end add menu", () => {
    const onShow = vi.fn();
    render(
      <ToolDockMenuProvider>
        <HiddenToolMenu
          cards={[{
            tool: "raven",
            title: "Raven",
            icon: null,
            fallback: Circle,
            tone: "#0153e5",
          }]}
          onShow={onShow}
          onResetOrder={vi.fn()}
        />
      </ToolDockMenuProvider>,
    );

    fireEvent.click(screen.getByRole("button", { name: "显示更多工具" }));
    expect(screen.getByText("未显示的工具")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("menuitem", { name: "显示 Raven" }));

    expect(onShow).toHaveBeenCalledWith("raven");
    expect(screen.queryByRole("menu")).toBeNull();
  });

  test("keeps add and default-order recovery available when no tools are hidden", () => {
    const onResetOrder = vi.fn();
    render(
      <ToolDockMenuProvider>
        <HiddenToolMenu
          cards={[]}
          onShow={vi.fn()}
          onResetOrder={onResetOrder}
        />
      </ToolDockMenuProvider>,
    );

    const addButton = screen.getByRole("button", { name: "显示更多工具" });
    expect(addButton).toBeEnabled();
    fireEvent.click(addButton);
    expect(screen.queryByText("未显示的工具")).toBeNull();
    fireEvent.click(screen.getByRole("menuitem", { name: "恢复默认顺序" }));
    expect(onResetOrder).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("menu")).toBeNull();
  });

  test("keeps the wide add menu inside the main content at narrow window widths", async () => {
    const originalInnerWidth = Object.getOwnPropertyDescriptor(window, "innerWidth");
    let viewportWidth = 502;
    let mainRight = 502;
    Object.defineProperty(window, "innerWidth", {
      configurable: true,
      get: () => viewportWidth,
    });
    const rect = (left: number, right: number): DOMRect => ({
      bottom: 80,
      height: 80,
      left,
      right,
      top: 0,
      width: right - left,
      x: left,
      y: 0,
      toJSON: () => ({}),
    });

    try {
      const { container } = render(
        <main>
          <ToolDockMenuProvider>
            <HiddenToolMenu
              cards={[{
                tool: "raven",
                title: "Raven",
                icon: null,
                fallback: Circle,
                tone: "#0153e5",
              }]}
              onShow={vi.fn()}
              onResetOrder={vi.fn()}
            />
          </ToolDockMenuProvider>
        </main>,
      );

      const main = container.querySelector("main") as HTMLElement;
      const dock = container.querySelector(".tool-dock-add") as HTMLDivElement;
      const addButton = screen.getByRole("button", { name: "显示更多工具" });
      vi.spyOn(main, "getBoundingClientRect").mockImplementation(() => rect(150, mainRight));
      vi.spyOn(dock, "getBoundingClientRect").mockReturnValue(rect(268, 332));
      vi.spyOn(addButton, "getBoundingClientRect").mockReturnValue(rect(276, 324));

      fireEvent.click(addButton);
      const menu = screen.getByRole("menu") as HTMLDivElement;
      expect(menu).toHaveStyle({ left: "-10px", width: "232px" });

      viewportWidth = 700;
      mainRight = 700;
      fireEvent(window, new Event("resize"));
      await waitFor(() => expect(menu).toHaveStyle({ left: "8px", width: "232px" }));
    } finally {
      if (originalInnerWidth) {
        Object.defineProperty(window, "innerWidth", originalInnerWidth);
      }
    }
  });

  test("keeps a long add menu inside the viewport and puts order recovery last", () => {
    const originalInnerHeight = Object.getOwnPropertyDescriptor(window, "innerHeight");
    Object.defineProperty(window, "innerHeight", {
      configurable: true,
      value: 160,
    });
    const rect = ({
      top,
      bottom,
      left,
      right,
    }: {
      top: number;
      bottom: number;
      left: number;
      right: number;
    }): DOMRect => ({
      top,
      bottom,
      left,
      right,
      height: bottom - top,
      width: right - left,
      x: left,
      y: top,
      toJSON: () => ({}),
    });

    try {
      const { container } = render(
        <ToolDockMenuProvider>
          <HiddenToolMenu
            cards={[{
              tool: "raven",
              title: "Raven",
              icon: null,
              fallback: Circle,
              tone: "#0153e5",
            }]}
            onShow={vi.fn()}
            onResetOrder={vi.fn()}
          />
        </ToolDockMenuProvider>,
      );

      const dock = container.querySelector(".tool-dock-add") as HTMLDivElement;
      const addButton = screen.getByRole("button", { name: "显示更多工具" });
      vi.spyOn(dock, "getBoundingClientRect").mockReturnValue(rect({
        top: 100,
        bottom: 148,
        left: 100,
        right: 164,
      }));
      vi.spyOn(addButton, "getBoundingClientRect").mockReturnValue(rect({
        top: 104,
        bottom: 144,
        left: 108,
        right: 156,
      }));

      fireEvent.click(addButton);
      const menu = screen.getByRole("menu");
      expect(menu).toHaveStyle({ top: "auto", bottom: "50px", maxHeight: "86px" });
      const menuItems = screen.getAllByRole("menuitem");
      expect(menuItems[menuItems.length - 1]).toHaveTextContent("恢复默认顺序");
    } finally {
      if (originalInnerHeight) {
        Object.defineProperty(window, "innerHeight", originalInnerHeight);
      }
    }
  });

  test("opens the dock-end add menu from right click and long press", () => {
    vi.useFakeTimers();
    try {
      render(
        <ToolDockMenuProvider>
          <HiddenToolMenu
            cards={[{
              tool: "raven",
              title: "Raven",
              icon: null,
              fallback: Circle,
              tone: "#0153e5",
            }]}
            onShow={vi.fn()}
            onResetOrder={vi.fn()}
          />
        </ToolDockMenuProvider>,
      );

      const addButton = screen.getByRole("button", { name: "显示更多工具" });
      fireEvent.contextMenu(addButton, { button: 2 });
      expect(screen.getByRole("menu")).toBeInTheDocument();

      fireEvent.pointerDown(document.body);
      expect(screen.queryByRole("menu")).toBeNull();

      fireEvent.pointerDown(addButton, {
        button: 0,
        pointerId: 7,
        clientX: 20,
        clientY: 20,
      });
      act(() => vi.advanceTimersByTime(500));
      expect(screen.getByRole("menu")).toBeInTheDocument();

      fireEvent.click(addButton);
      expect(screen.getByRole("menu")).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });

  test("reveals the icon with a bounded sector and shows operation text below it", () => {
    render(
      <ToolDockMenuProvider>
        <QuickCard
          id="codex-progress"
          title="Codex"
          icon={null}
          fallback={Circle}
          tone="#111827"
          statusText={"迁移会话\n12/48"}
          statusTone="pending"
          actionLabel="配置"
          busy
          disabled
          progress={0.25}
          onAction={vi.fn()}
          onConfigure={vi.fn()}
          onRemoveConfig={vi.fn()}
          onLocate={vi.fn()}
        />
      </ToolDockMenuProvider>,
    );

    const progress = screen.getByRole("progressbar", { name: /Codex 迁移会话\s+12\/48/ });
    expect(progress).toHaveAttribute("aria-valuenow", "25");
    expect(progress).toHaveClass("tool-dock-progress-reveal");
    expect(progress.tagName).toBe("svg");
    expect(progress).toHaveAttribute("viewBox", "0 0 48 48");
    const toolButton = screen.getByRole("button", { name: "Codex" });
    expect(toolButton).toHaveStyle("--tool-accent: #111827");
    expect(toolButton).toContainElement(progress);
    expect(progress.querySelector("mask rect")).toHaveAttribute("fill", "white");
    expect(progress.querySelector("mask rect")).toHaveAttribute("rx", "9");
    expect(progress.querySelector(".tool-dock-progress-sector"))
      .toHaveAttribute("stroke", "black");
    expect(progress.querySelector(".tool-dock-progress-sector"))
      .toHaveAttribute("stroke-dasharray", "0.25 0.75");
    expect(progress.querySelector(".tool-dock-progress-glass-rim"))
      .toHaveAttribute("rx", "8.25");
    expect(progress.querySelector(".tool-dock-progress-edge"))
      .toHaveStyle("transform: rotate(90deg)");
    const liveStatus = screen.getByLabelText(/迁移会话\s+12\/48/, { selector: "span" });
    expect(liveStatus).toHaveClass("tool-dock-status-copy");
    expect(liveStatus.closest(".tool-dock-status-text")).toHaveClass("is-live");
    expect(liveStatus.querySelector(".tool-dock-status-label")).toHaveTextContent("迁移会话");
    expect(liveStatus.querySelector(".tool-dock-status-count")).toHaveTextContent("12/48");
    expect(document.querySelector(".tool-dock-hover-name")).toBeNull();
    expect(document.querySelector(".tool-dock-center-spinner")).toBeNull();
    expect(document.querySelector(".tool-dock-progress-ring")).toBeNull();
  });
});

describe("CompactChoiceMenu", () => {
  test("uses an app-rendered popup and changes selection explicitly", () => {
    const onChange = vi.fn();
    render(
      <CompactChoiceMenu
        ariaLabel="本地 API 公开面"
        value="openai"
        options={[
          { value: "openai", label: "OpenAI" },
          { value: "anthropic", label: "Anthropic" },
          { value: "gemini", label: "Gemini" },
        ]}
        onChange={onChange}
      />,
    );

    const trigger = screen.getByRole("button", { name: "本地 API 公开面" });
    expect(document.querySelector("select")).toBeNull();
    fireEvent.click(trigger);
    expect(screen.getByRole("listbox", { name: "本地 API 公开面" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("option", { name: "Anthropic" }));

    expect(onChange).toHaveBeenCalledWith("anthropic");
    expect(screen.queryByRole("listbox")).toBeNull();
  });

  test("supports keyboard opening, navigation, and explicit dismissal", async () => {
    render(
      <CompactChoiceMenu
        ariaLabel="协议"
        value="responses"
        options={[
          { value: "responses", label: "Responses" },
          { value: "chat", label: "Chat" },
        ]}
        onChange={vi.fn()}
      />,
    );

    const trigger = screen.getByRole("button", { name: "协议" });
    fireEvent.keyDown(trigger, { key: "ArrowDown" });
    const firstOption = screen.getByRole("option", { name: "Responses" });
    fireEvent.keyDown(firstOption, { key: "ArrowDown" });
    fireEvent.keyDown(screen.getByRole("option", { name: "Chat" }), { key: "Escape" });

    expect(screen.queryByRole("listbox")).toBeNull();
    await waitFor(() => expect(trigger).toHaveFocus());
  });

  test("closes from an outside click even when a parent menu stops bubbling", () => {
    render(
      <div onPointerDown={(event) => event.stopPropagation()}>
        <CompactChoiceMenu
          ariaLabel="协议"
          value="responses"
          options={[
            { value: "responses", label: "Responses" },
            { value: "chat", label: "Chat" },
          ]}
          onChange={vi.fn()}
        />
        <div data-testid="parent-menu-blank">菜单空白区域</div>
      </div>,
    );

    fireEvent.click(screen.getByRole("button", { name: "协议" }));
    expect(screen.getByRole("listbox")).toBeInTheDocument();
    fireEvent.pointerDown(screen.getByTestId("parent-menu-blank"));

    expect(screen.queryByRole("listbox")).toBeNull();
  });
});
