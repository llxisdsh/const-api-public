// @vitest-environment jsdom
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, test, vi } from "vitest";
import { ChannelPriorityDialog } from "./ChannelPriorityDialog";

const channels = [
  { id: "a", name: "Account A", local_priority: 1 },
  { id: "b", name: "Account B", local_priority: 9 },
  { id: "c", name: "Backup", local_priority: 20 },
];

describe("ChannelPriorityDialog", () => {
  test("edits a distant channel directly, keeps rows still, and saves only its priority", async () => {
    const onSave = vi.fn().mockResolvedValue(true), onClose = vi.fn();
    render(<ChannelPriorityDialog channels={channels} initialChannelId="b" onSave={onSave} onClose={onClose} />);
    const field = screen.getByRole("spinbutton", { name: "Account B 的优先级" });
    expect(document.activeElement).toBe(field);
    fireEvent.change(field, { target: { value: "1" } });
    expect(onSave).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(onClose).toHaveBeenCalledOnce());
    expect(onSave).toHaveBeenCalledExactlyOnceWith({ b: 1 });
  });

  test("batch edits selected accounts and cancel never persists", () => {
    const onSave = vi.fn(), onClose = vi.fn();
    render(<ChannelPriorityDialog channels={channels} onSave={onSave} onClose={onClose} />);
    fireEvent.click(screen.getByRole("checkbox", { name: "选择 Account B" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "选择 Backup" }));
    fireEvent.click(screen.getByRole("button", { name: "应用" }));
    expect((screen.getByRole("spinbutton", { name: "Backup 的优先级" }) as HTMLInputElement).value).toBe("1");
    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    expect(onSave).not.toHaveBeenCalled();
    expect(onClose).toHaveBeenCalledOnce();
  });

  test("invalid priorities cannot be saved, and failed saves leave the dialog open", async () => {
    const onSave = vi.fn().mockResolvedValue(false), onClose = vi.fn();
    render(<ChannelPriorityDialog channels={channels} onSave={onSave} onClose={onClose} />);
    const field = screen.getByRole("spinbutton", { name: "Account A 的优先级" });
    fireEvent.change(field, { target: { value: "0" } });
    expect(screen.getByRole("alert").textContent).toContain("整数");
    expect((screen.getByRole("button", { name: "保存" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.change(field, { target: { value: "3" } });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(onSave).toHaveBeenCalledOnce());
    expect(onClose).not.toHaveBeenCalled();
  });
});
