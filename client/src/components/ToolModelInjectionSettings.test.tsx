import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { useState, type ComponentProps } from "react";
import { ToolModelInjectionSettings } from "./ToolModelInjectionSettings";
import { DEFAULT_TOOL_MODEL_INJECTION } from "../toolModelInjection";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

function Settings(props: Omit<ComponentProps<typeof ToolModelInjectionSettings>, "open" | "onOpenChange">) {
  const [open, setOpen] = useState(false);
  const [settings, setSettings] = useState(props.settings);
  const onSave: typeof props.onSave = async (...args) => {
    const result = await props.onSave(...args);
    setSettings(args[0]);
    return result;
  };
  return <ToolModelInjectionSettings {...props} settings={settings} onSave={onSave} open={open} onOpenChange={setOpen} />;
}

describe("tool model injection settings", () => {
  beforeEach(() => vi.mocked(invoke).mockReset().mockResolvedValue({ patterns: ["gpt-*", "claude-*"], available_models: ["local-model", "extra-model"] }));

  it("defaults to all; reads recommendations only when opened and only permits appending/removing additions", async () => {
    const onSave = vi.fn().mockResolvedValue({ synced: 2, errors: [] });
    render(<Settings settings={DEFAULT_TOOL_MODEL_INJECTION} onSave={onSave} />);
    expect(screen.getByRole("checkbox", { name: /注入全部模型/ })).toBeChecked();
    expect(invoke).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("checkbox"));
    await waitFor(() => expect(screen.getByRole("checkbox")).not.toBeChecked());
    expect(onSave).toHaveBeenCalledWith({ all_models: false, extra_models: [] }, expect.any(Function), false);
    expect(screen.queryByRole("button", { name: "保存并同步" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "推荐列表" }));
    await screen.findByText("gpt-*");
    expect(screen.queryByRole("button", { name: /上移|下移|隐藏/ })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "删除追加项 gpt-*" })).not.toBeInTheDocument();
    const input = screen.getByRole("combobox", { name: "追加模型或前缀" });
    fireEvent.change(input, { target: { value: "gpt-*" } });
    expect(screen.getByRole("button", { name: "追加" })).toBeDisabled();
    for (const value of [" EXTRA-Z ", "extra-a"]) {
      fireEvent.change(input, { target: { value } });
      fireEvent.click(screen.getByRole("button", { name: "追加" }));
    }
    fireEvent.click(screen.getByRole("button", { name: "删除追加项 extra-z" }));
    fireEvent.click(screen.getByRole("button", { name: "保存并同步" }));
    await waitFor(() => expect(onSave).toHaveBeenCalledWith({ all_models: false, extra_models: ["extra-a"] }, expect.any(Function)));
    expect(await screen.findByRole("status")).toHaveTextContent("已保存，已同步 2 个工具");
    expect(screen.getByRole("button", { name: "保存并同步" })).toBeDisabled();
  });

  it("enables save only for real list changes and keeps each delete action in its model row", async () => {
    const onSave = vi.fn().mockResolvedValue({ synced: 1, errors: [] });
    render(<Settings settings={{ all_models: false, extra_models: ["mine"] }} onSave={onSave} />);
    fireEvent.click(screen.getByRole("button", { name: "推荐列表" }));
    await screen.findByText("gpt-*");
    const save = screen.getByRole("button", { name: "保存并同步" });
    expect(save).toBeDisabled();
    const input = screen.getByRole("combobox", { name: "追加模型或前缀" });
    fireEvent.change(input, { target: { value: "extra" } });
    fireEvent.click(screen.getByRole("button", { name: "追加" }));
    expect(save).toBeEnabled();
    const rows = within(screen.getByRole("list", { name: "我的追加" })).getAllByRole("listitem");
    expect(rows).toHaveLength(2);
    expect(within(rows[1]).getByRole("button", { name: "删除追加项 extra" })).toBeInTheDocument();
    expect(rows[1].children).toHaveLength(3);
    fireEvent.click(screen.getByRole("button", { name: "删除追加项 extra" }));
    expect(save).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "恢复默认" }));
    expect(save).toBeEnabled();
    expect(onSave).not.toHaveBeenCalled();
  });

  it("auto-saves only the switch, preserving unconfirmed additions for the editor", async () => {
    const onSave = vi.fn().mockResolvedValue({ synced: 0, errors: [] });
    render(<Settings settings={DEFAULT_TOOL_MODEL_INJECTION} onSave={onSave} />);
    fireEvent.click(screen.getByRole("button", { name: "推荐列表" }));
    await screen.findByText("gpt-*");
    fireEvent.change(screen.getByRole("combobox", { name: "追加模型或前缀" }), { target: { value: "draft-only" } });
    fireEvent.click(screen.getByRole("button", { name: "追加" }));
    fireEvent.click(screen.getByRole("button", { name: "关闭" }));
    expect(screen.queryByRole("button", { name: "保存并同步" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("checkbox"));
    await waitFor(() => expect(screen.getByRole("checkbox")).not.toBeChecked());
    expect(onSave).toHaveBeenCalledWith({ all_models: false, extra_models: [] }, expect.any(Function), false);
    fireEvent.click(screen.getByRole("button", { name: "推荐列表" }));
    expect(screen.getByText("draft-only")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "保存并同步" })).toBeEnabled();
  });

  it("keeps the saved switch value after a save error and permits retry without an extra save button", async () => {
    const onSave = vi.fn().mockRejectedValueOnce(new Error("fixture save failure")).mockResolvedValue({ synced: 0, errors: [] });
    render(<Settings settings={DEFAULT_TOOL_MODEL_INJECTION} onSave={onSave} />);
    fireEvent.click(screen.getByRole("checkbox"));
    expect(await screen.findByRole("alert")).toHaveTextContent("fixture save failure");
    expect(screen.getByRole("checkbox")).toBeChecked();
    expect(screen.queryByRole("button", { name: "保存并同步" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("checkbox"));
    await waitFor(() => expect(screen.getByRole("checkbox")).not.toBeChecked());
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("supports an empty local recommendation list, restoring only additions, and retrying sync failures inline", async () => {
    vi.mocked(invoke).mockResolvedValue({ patterns: [], available_models: ["local-model"] });
    const onSave = vi.fn().mockResolvedValueOnce({ synced: 0, errors: ["TRAE: fixture failure"] }).mockResolvedValue({ synced: 1, errors: [] });
    render(<Settings settings={{ all_models: false, extra_models: ["local-model"] }} onSave={onSave} />);
    fireEvent.click(screen.getByRole("button", { name: "推荐列表" }));
    await screen.findByText("暂无推荐规则，可在下方追加。");
    fireEvent.click(screen.getByRole("button", { name: "恢复默认" }));
    expect(screen.queryByRole("button", { name: "删除追加项 local-model" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "保存并同步" }));
    await screen.findByRole("alert");
    expect(onSave).toHaveBeenCalledWith({ all_models: false, extra_models: [] }, expect.any(Function));
    expect(screen.getByRole("alert")).toHaveTextContent("TRAE: fixture failure");
    fireEvent.click(screen.getByRole("button", { name: "保存并同步" }));
    await waitFor(() => expect(screen.queryByRole("alert")).not.toBeInTheDocument());
    expect(await screen.findByRole("status")).toHaveTextContent("已保存，已同步 1 个工具");
  });

  it("keeps a load failure distinct from an empty list and can refresh without losing additions", async () => {
    vi.mocked(invoke).mockRejectedValueOnce(new Error("offline"));
    render(<Settings settings={{ all_models: false, extra_models: ["mine"] }} onSave={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "推荐列表" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("offline");
    expect(screen.queryByText("暂无推荐规则，可在下方追加。")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    await screen.findByText("gpt-*");
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(screen.getByText("mine")).toBeInTheDocument();
  });
});
