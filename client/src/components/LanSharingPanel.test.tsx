// @vitest-environment jsdom

import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, test, vi } from "vitest";

import type { LanShareHostStatus } from "../appTypes";
import { changeAppLanguage } from "../i18n";
import { LanSharingDrawer } from "./LanSharingPanel";

const invoke = vi.hoisted(() => vi.fn());
const askConfirm = vi.fn();
const emptyPresentation = { schema_version: 1, rules: [] };

vi.mock("@tauri-apps/api/core", () => ({ invoke }));

const hostStatus: LanShareHostStatus = {
  allow_lan_access: true,
  listen: "0.0.0.0:38788",
  connect_url: "http://192.168.1.20:38788",
  connect_addresses: [
    {
      interface_name: "Ethernet",
      ip: "192.168.1.20",
      url: "http://192.168.1.20:38788",
      is_primary: true,
    },
    {
      interface_name: "Tailscale",
      ip: "100.64.0.8",
      url: "http://100.64.0.8:38788",
      is_primary: false,
    },
  ],
  shared_models: ["model-a"],
  available_models: ["model-a", "model-b"],
  input_weight: 1,
  output_weight: 4,
  members: [{
    id: "member-a",
    name: "Alice",
    api_key: "sk-lan-0123456789abcdef0123456789abcdef",
    enabled: true,
    weekly_token_limit: 10_000,
    remaining_tokens: 7_000,
    exhausted: false,
    week_started_at_unix: 1_700_000_000,
    resets_at_unix: 1_700_604_800,
    input_tokens: 1_000,
    output_tokens: 500,
    used_tokens: 3_000,
    requests: 12,
  }],
};

describe("LanSharingDrawer", () => {
  beforeEach(async () => {
    await changeAppLanguage("zh-CN");
    invoke.mockReset();
    askConfirm.mockReset();
    askConfirm.mockResolvedValue(true);
    invoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
      if (command === "get_model_presentation") return emptyPresentation;
      if (command === "set_lan_share_connect_address") {
        return { ...hostStatus, connect_url: args?.connectUrl as string };
      }
      if (command === "set_lan_share_models") {
        return { ...hostStatus, shared_models: [...(args?.models as string[])] };
      }
      if (command === "update_lan_share_member") {
        return {
          ...hostStatus,
          members: hostStatus.members.map((member) => member.id === args?.memberId ? {
            ...member,
            name: args.name as string,
            weekly_token_limit: args.weeklyTokenLimit as number,
            enabled: args.enabled as boolean,
          } : member),
        };
      }
      if (command === "reset_lan_share_member") {
        return {
          ...hostStatus,
          members: hostStatus.members.map((member) => ({
            ...member,
            input_tokens: 0,
            output_tokens: 0,
            used_tokens: 0,
            requests: 0,
            remaining_tokens: member.weekly_token_limit,
          })),
        };
      }
      if (command === "create_lan_share_member") {
        return {
          ...hostStatus,
          members: [{
            ...hostStatus.members[0],
            id: "member-new",
            name: args?.name as string,
            api_key: "sk-lan-new-member",
            weekly_token_limit: args?.weeklyTokenLimit as number,
            remaining_tokens: args?.weeklyTokenLimit as number,
            input_tokens: 0,
            output_tokens: 0,
            used_tokens: 0,
            requests: 0,
          }, ...hostStatus.members],
        };
      }
      return hostStatus;
    });
  });

  test("uses the standard detail drawer and saves models independently", async () => {
    const user = userEvent.setup();
    const { container } = render(
      <LanSharingDrawer
        enabled
        proxyRunning
        onToggle={vi.fn()}
        copyText={vi.fn().mockResolvedValue(true)}
        askConfirm={askConfirm}
        onClose={vi.fn()}
      />,
    );

    expect(await screen.findByRole("dialog", { name: "局域网共享" })).toHaveClass("drawer-backdrop");
    expect(container.querySelector(".detail-drawer.lan-share-drawer")).toBeInTheDocument();
    expect(container.querySelector("#lan-share-description")).toHaveTextContent(
      "局域网共享独立于模型市场：不会影响渠道的上线状态，也不能使用模型市场中其他用户提供的模型",
    );
    expect(screen.getByRole("button", { name: "停止局域网共享" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "保存模型" })).not.toBeInTheDocument();

    await user.click(screen.getByRole("checkbox", { name: "model-b" }));
    expect(screen.getByRole("button", { name: "保存模型" })).toBeInTheDocument();
    expect(invoke).toHaveBeenCalledTimes(2);
    expect(invoke).toHaveBeenCalledWith("get_lan_share_status");
    expect(invoke).toHaveBeenCalledWith("get_model_presentation");

    await user.click(screen.getByRole("button", { name: "保存模型" }));
    await waitFor(() => expect(invoke).toHaveBeenLastCalledWith("set_lan_share_models", {
      models: ["model-a", "model-b"],
    }));
    expect(screen.queryByRole("button", { name: "保存模型" })).not.toBeInTheDocument();
  });

  test("names the disabled sharing action explicitly", async () => {
    render(
      <LanSharingDrawer
        enabled={false}
        proxyRunning
        onToggle={vi.fn()}
        copyText={vi.fn().mockResolvedValue(true)}
        askConfirm={askConfirm}
        onClose={vi.fn()}
      />,
    );

    expect(await screen.findByRole("button", { name: "开启局域网共享" })).toBeInTheDocument();
  });

  test("shows unique short names but preserves selected upstream identities and short invite labels", async () => {
    const user = userEvent.setup();
    const copyText = vi.fn().mockResolvedValue(true);
    let status = {
      ...hostStatus,
      shared_models: ["Shared/Mixed"],
      available_models: ["private/Mixed", "Shared/Mixed", "google/Gemini-3-Pro-Image", "vendor/branch/New:free"],
    };
    invoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
      if (command === "get_model_presentation") return emptyPresentation;
      if (command === "set_lan_share_models") status = { ...status, shared_models: args?.models as string[] };
      return status;
    });
    render(<LanSharingDrawer enabled proxyRunning onToggle={vi.fn()} copyText={copyText} askConfirm={askConfirm} onClose={vi.fn()} />);

    expect(await screen.findByRole("checkbox", { name: "mixed" })).toBeChecked();
    expect(screen.getAllByRole("checkbox").map((checkbox) => checkbox.closest("label")?.textContent))
      .toEqual(["gemini-3-pro-image", "mixed", "new:free"]);
    expect(screen.queryByText("google/Gemini-3-Pro-Image")).not.toBeInTheDocument();
    expect(screen.getByText("已选 1 个，共 3 个")).toBeInTheDocument();

    await user.click(screen.getByRole("checkbox", { name: "gemini-3-pro-image" }));
    await user.click(screen.getByRole("button", { name: "保存模型" }));
    await waitFor(() => expect(invoke).toHaveBeenLastCalledWith("set_lan_share_models", {
      models: ["Shared/Mixed", "google/Gemini-3-Pro-Image"],
    }));
    await user.click(screen.getByRole("button", { name: /Alice/ }));
    await user.click(screen.getByRole("button", { name: "复制连接信息" }));
    expect(copyText.mock.calls[0]?.[0]).toContain("可用模型: gemini-3-pro-image, mixed");
    expect(copyText.mock.calls[0]?.[0]).not.toContain("Shared/Mixed");

    await user.click(screen.getByRole("button", { name: "全选" }));
    await user.click(screen.getByRole("button", { name: "保存模型" }));
    await waitFor(() => expect(invoke).toHaveBeenLastCalledWith("set_lan_share_models", {
      models: ["google/Gemini-3-Pro-Image", "Shared/Mixed", "vendor/branch/New:free"],
    }));
  });

  test("unchecking a short name removes hidden legacy duplicates instead of leaving them shared", async () => {
    const user = userEvent.setup();
    const status = { ...hostStatus, available_models: ["a/Mixed", "b/Mixed"], shared_models: ["a/Mixed", "b/Mixed"] };
    invoke.mockImplementation(async (command: string) => command === "get_model_presentation" ? emptyPresentation : status);
    render(<LanSharingDrawer enabled proxyRunning onToggle={vi.fn()} copyText={vi.fn()} askConfirm={askConfirm} onClose={vi.fn()} />);

    expect(await screen.findByRole("checkbox", { name: "mixed" })).toBeChecked();
    expect(screen.getByText("已选 1 个，共 1 个")).toBeInTheDocument();
    await user.click(screen.getByRole("checkbox", { name: "mixed" }));
    await user.click(screen.getByRole("button", { name: "保存模型" }));
    await waitFor(() => expect(invoke).toHaveBeenLastCalledWith("set_lan_share_models", { models: [] }));
  });

  test("refreshes server ordering without clearing drafts or fetching rules on usage polls", async () => {
    const user = userEvent.setup();
    const interval = vi.spyOn(window, "setInterval");
    const warning = vi.spyOn(console, "warn").mockImplementation(() => {});
    let failRules = false;
    let rules = [{ pattern: "gpt-*", hidden: true }];
    let status = {
      ...hostStatus,
      available_models: ["private/GPT-Z", "Shared/GPT-Z", "anthropic/Claude-A", "vendor/alpha", "openai/gpt-a"],
      shared_models: ["Shared/GPT-Z"],
    };
    invoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
      if (command === "get_model_presentation") {
        if (failRules) throw new Error("catalog offline");
        return { schema_version: 1, rules };
      }
      if (command === "set_lan_share_models") status = { ...status, shared_models: args?.models as string[] };
      return status;
    });
    const { unmount } = render(<LanSharingDrawer enabled proxyRunning onToggle={vi.fn()} copyText={vi.fn()} askConfirm={askConfirm} onClose={vi.fn()} />);
    const labels = () => screen.getAllByRole("checkbox").map((checkbox) => checkbox.closest("label")?.textContent);
    try {
      expect(await screen.findByRole("checkbox", { name: "gpt-z" })).toBeChecked();
      expect(labels()).toEqual(["gpt-a", "gpt-z", "alpha", "claude-a"]);
      await user.click(screen.getByRole("checkbox", { name: "claude-a" }));
      rules = [{ pattern: "claude-*", hidden: false }];
      await user.click(screen.getByRole("button", { name: "刷新" }));
      await waitFor(() => expect(labels()).toEqual(["claude-a", "alpha", "gpt-a", "gpt-z"]));
      expect(screen.getByRole("checkbox", { name: "claude-a" })).toBeChecked();
      expect(screen.getByRole("checkbox", { name: "gpt-z" })).toBeChecked();

      const poll = interval.mock.calls.find(([, delay]) => delay === 5000)?.[0] as () => void;
      await act(async () => { poll(); });
      expect(invoke.mock.calls.filter(([command]) => command === "get_model_presentation")).toHaveLength(2);
      expect(invoke.mock.calls.filter(([command]) => command === "get_lan_share_status")).toHaveLength(3);

      failRules = true;
      await user.click(screen.getByRole("button", { name: "刷新" }));
      await waitFor(() => expect(warning).toHaveBeenCalledWith(expect.stringContaining("retaining current order"), expect.any(Error)));
      expect(labels()).toEqual(["claude-a", "alpha", "gpt-a", "gpt-z"]);
      await user.click(screen.getByRole("button", { name: "保存模型" }));
      await waitFor(() => expect(invoke).toHaveBeenLastCalledWith("set_lan_share_models", {
        models: ["Shared/GPT-Z", "anthropic/Claude-A"],
      }));
    } finally {
      unmount();
      interval.mockRestore();
      warning.mockRestore();
    }
  });

  test("keeps members collapsed and shows save only inside the changed member", async () => {
    const user = userEvent.setup();
    const { container } = render(
      <LanSharingDrawer
        enabled
        proxyRunning
        onToggle={vi.fn()}
        copyText={vi.fn().mockResolvedValue(true)}
        askConfirm={askConfirm}
        onClose={vi.fn()}
      />,
    );

    await screen.findByText("Alice");
    const addMemberForm = container.querySelector<HTMLElement>(".lan-share-add-member");
    expect(addMemberForm).not.toBeNull();
    expect(within(addMemberForm!).getByRole("spinbutton", { name: "每周 Token 上限" })).toHaveAccessibleDescription(
      "输入、输出 Token 优先采用模型返回值；缺少哪一项，就按对应请求或响应每 4 字节约算 1 Token。每周额度按“输入 Token × 1 + 输出 Token × 4”扣除。",
    );
    expect(within(addMemberForm!).getByText(/优先采用模型返回值/)).toHaveClass("lan-share-add-member-weight-hint");
    const memberListHeader = container.querySelector<HTMLElement>(".lan-share-member-list-header");
    expect(memberListHeader).not.toBeNull();
    expect(within(memberListHeader!).getByText("名称")).toBeInTheDocument();
    expect(within(memberListHeader!).getByText("状态")).toBeInTheDocument();
    expect(within(memberListHeader!).getByText("周用量")).toBeInTheDocument();
    const member = container.querySelector<HTMLElement>(".lan-share-member");
    expect(member).not.toBeNull();
    const memberSummary = member!.querySelector<HTMLElement>(".lan-share-member-disclosure");
    expect(memberSummary).not.toBeNull();
    expect(memberSummary!.children).toHaveLength(4);
    expect(within(memberSummary!).getByText("Alice")).toBeInTheDocument();
    expect(within(memberSummary!).getByText("启用")).toBeInTheDocument();
    expect(within(memberSummary!).getByText("3,000 / 10,000")).toBeInTheDocument();
    expect(within(member!).queryByLabelText("成员名称")).not.toBeInTheDocument();
    expect(within(member!).queryByRole("button", { name: "保存成员" })).not.toBeInTheDocument();

    await user.click(within(member!).getByRole("button", { name: /Alice/ }));
    expect(within(member!).queryByText(/优先采用模型返回值/)).not.toBeInTheDocument();
    const nameInput = within(member!).getByLabelText("成员名称");
    await user.clear(nameInput);
    await user.type(nameInput, "Alice desk");
    expect(within(member!).getByRole("button", { name: "保存成员" })).toBeInTheDocument();

    await user.click(within(member!).getByRole("button", { name: "保存成员" }));
    await waitFor(() => expect(invoke).toHaveBeenLastCalledWith("update_lan_share_member", {
      memberId: "member-a",
      name: "Alice desk",
      weeklyTokenLimit: 10_000,
      enabled: true,
    }));
  });

  test("copies the address selected from multiple network adapters", async () => {
    const user = userEvent.setup();
    const copyText = vi.fn().mockResolvedValue(true);
    const { container } = render(
      <LanSharingDrawer
        enabled
        proxyRunning
        onToggle={vi.fn()}
        copyText={copyText}
        askConfirm={askConfirm}
        onClose={vi.fn()}
      />,
    );

    await screen.findByText("Alice");
    await user.click(screen.getByRole("button", { name: "选择连接地址" }));
    await user.click(screen.getByRole("option", { name: /Tailscale/ }));
    await user.click(screen.getByRole("button", { name: "保存地址" }));
    await waitFor(() => expect(invoke).toHaveBeenLastCalledWith("set_lan_share_connect_address", {
      connectUrl: "http://100.64.0.8:38788",
    }));
    const member = container.querySelector<HTMLElement>(".lan-share-member");
    await user.click(within(member!).getByRole("button", { name: /Alice/ }));
    await user.click(within(member!).getByRole("button", { name: "复制连接信息" }));

    const copiedConnection = copyText.mock.calls[0]?.[0] as string;
    expect(copiedConnection).toContain("渠道名称: 局域网共享 · 100.64.0.8");
    expect(copiedConnection).toContain("Base URL: http://100.64.0.8:38788/v1");
    expect(copiedConnection).toContain("API Key: sk-lan-0123456789abcdef0123456789abcdef");
    expect(copiedConnection).toContain("使用方法:");
    expect(copiedConnection).toContain("把这整段信息粘贴到“粘贴连接信息”");
    expect(copiedConnection).toContain("点击“验证并保存”，成功后点击“启用渠道”");
    expect(copyText).toHaveBeenCalledWith(copiedConnection, "成员连接信息已复制");
  });

  test("confirms a usage reset and shows success even when usage becomes zero", async () => {
    const user = userEvent.setup();
    const { container } = render(
      <LanSharingDrawer
        enabled
        proxyRunning
        onToggle={vi.fn()}
        copyText={vi.fn().mockResolvedValue(true)}
        askConfirm={askConfirm}
        onClose={vi.fn()}
      />,
    );

    await screen.findByText("Alice");
    const member = container.querySelector<HTMLElement>(".lan-share-member");
    await user.click(within(member!).getByRole("button", { name: /Alice/ }));
    await user.click(within(member!).getByRole("button", { name: "重置用量" }));

    expect(askConfirm).toHaveBeenCalledWith({
      title: "重置本周用量？",
      message: "将清空“Alice”本周已统计的 Token 用量，操作后不可恢复。",
      confirmText: "确认重置",
      tone: "danger",
    });
    await waitFor(() => expect(invoke).toHaveBeenLastCalledWith("reset_lan_share_member", {
      memberId: "member-a",
    }));
    expect(within(member!).getByRole("button", { name: "已重置" })).toBeInTheDocument();
    expect(within(member!).getByText("0 / 10,000")).toBeInTheDocument();
  });

  test("puts cancel in the original slot before confirming key replacement or deletion", async () => {
    const user = userEvent.setup();
    const { container } = render(
      <LanSharingDrawer
        enabled
        proxyRunning
        onToggle={vi.fn()}
        copyText={vi.fn().mockResolvedValue(true)}
        askConfirm={askConfirm}
        onClose={vi.fn()}
      />,
    );

    await screen.findByText("Alice");
    const member = container.querySelector<HTMLElement>(".lan-share-member");
    expect(member).not.toBeNull();
    await user.click(within(member!).getByRole("button", { name: /Alice/ }));
    const actions = member!.querySelector<HTMLElement>(".lan-share-member-actions");
    expect(actions).not.toBeNull();

    let buttons = within(actions!).getAllByRole("button");
    const regenerateIndex = buttons.findIndex((button) => button.textContent?.includes("更换 Key"));
    expect(regenerateIndex).toBeGreaterThanOrEqual(0);
    await user.click(buttons[regenerateIndex]);

    buttons = within(actions!).getAllByRole("button");
    expect(buttons[regenerateIndex]).toHaveAccessibleName("取消");
    expect(buttons[regenerateIndex + 1]).toHaveAccessibleName("确认更换");
    await user.click(buttons[regenerateIndex]);
    expect(invoke).not.toHaveBeenCalledWith("regenerate_lan_share_member_key", expect.anything());

    buttons = within(actions!).getAllByRole("button");
    const deleteIndex = buttons.findIndex((button) => button.textContent?.includes("删除"));
    expect(deleteIndex).toBeGreaterThanOrEqual(0);
    await user.click(buttons[deleteIndex]);

    buttons = within(actions!).getAllByRole("button");
    expect(buttons[deleteIndex]).toHaveAccessibleName("取消");
    expect(buttons[deleteIndex + 1]).toHaveAccessibleName("确认删除");
    await user.click(buttons[deleteIndex]);
    expect(invoke).not.toHaveBeenCalledWith("delete_lan_share_member", expect.anything());
  });

  test("keeps only one member editor expanded in a long list", async () => {
    const user = userEvent.setup();
    invoke.mockImplementation(async (command: string) => command === "get_model_presentation" ? emptyPresentation : {
      ...hostStatus,
      members: [
        hostStatus.members[0],
        { ...hostStatus.members[0], id: "member-b", name: "Bob", api_key: "cst-lan-bob" },
      ],
    });
    const { container } = render(
      <LanSharingDrawer
        enabled
        proxyRunning
        onToggle={vi.fn()}
        copyText={vi.fn().mockResolvedValue(true)}
        askConfirm={askConfirm}
        onClose={vi.fn()}
      />,
    );

    await screen.findByText("Bob");
    const members = container.querySelectorAll<HTMLElement>(".lan-share-member");
    await user.click(within(members[0]).getByRole("button", { name: /Alice/ }));
    expect(within(members[0]).getByLabelText("成员名称")).toBeInTheDocument();

    await user.click(within(members[1]).getByRole("button", { name: /Bob/ }));
    expect(within(members[0]).queryByLabelText("成员名称")).not.toBeInTheDocument();
    expect(within(members[1]).getByLabelText("成员名称")).toBeInTheDocument();
  });

  test("expands a newly added member and collapses the previously open member", async () => {
    const user = userEvent.setup();
    const { container } = render(
      <LanSharingDrawer
        enabled
        proxyRunning
        onToggle={vi.fn()}
        copyText={vi.fn().mockResolvedValue(true)}
        askConfirm={askConfirm}
        onClose={vi.fn()}
      />,
    );

    await screen.findByText("Alice");
    const originalMember = container.querySelector<HTMLElement>(".lan-share-member");
    expect(originalMember).not.toBeNull();
    await user.click(within(originalMember!).getByRole("button", { name: /Alice/ }));
    expect(within(originalMember!).getByLabelText("成员名称")).toBeInTheDocument();

    const addMemberForm = container.querySelector<HTMLElement>(".lan-share-add-member");
    expect(addMemberForm).not.toBeNull();
    const addMemberButton = within(addMemberForm!).getByRole("button", { name: "添加成员" });
    const addMemberNameInput = within(addMemberForm!).getByRole("textbox");
    expect(addMemberButton).toHaveClass("primary");
    expect(addMemberButton).toBeEnabled();
    expect(addMemberNameInput).toBeRequired();
    await user.click(addMemberButton);
    expect(within(addMemberForm!).getByRole("alert")).toHaveTextContent("请先填写成员名称。");
    expect(addMemberNameInput).toHaveAttribute("aria-invalid", "true");
    expect(invoke).not.toHaveBeenCalledWith("create_lan_share_member", expect.anything());

    await user.type(addMemberNameInput, "Bob");
    expect(within(addMemberForm!).queryByRole("alert")).not.toBeInTheDocument();
    await user.click(addMemberButton);

    await screen.findByText("Bob");
    const members = [...container.querySelectorAll<HTMLElement>(".lan-share-member")];
    const aliceMember = members.find((member) => within(member).queryByText("Alice"));
    const bobMember = members.find((member) => within(member).queryByText("Bob"));
    expect(aliceMember).toBeDefined();
    expect(bobMember).toBeDefined();
    expect(within(aliceMember!).queryByLabelText("成员名称")).not.toBeInTheDocument();
    expect(within(bobMember!).getByLabelText("成员名称")).toBeInTheDocument();
    expect(container.querySelectorAll(".lan-share-member.expanded")).toHaveLength(1);
  });
});
