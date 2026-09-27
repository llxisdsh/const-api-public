// @vitest-environment jsdom

import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import {
  TOOL_DOCK_CLOSE_DELAY_MS,
  ToolDockMenuProvider,
  useToolDockMenu,
} from "./ToolDockMenu";

function MenuProbe({ id }: { id: string }) {
  const menu = useToolDockMenu();
  return (
    <div className="tool-dock">
      <button type="button" onClick={() => menu.openMenu(id)}>open {id}</button>
      <button type="button" onClick={() => menu.togglePinnedMenu(id)}>pin {id}</button>
      <button type="button" onClick={menu.scheduleClose}>leave {id}</button>
      <span data-testid={`${id}-state`}>{menu.openMenuId ?? "closed"}</span>
    </div>
  );
}

describe("ToolDockMenuProvider", () => {
  afterEach(() => vi.useRealTimers());

  test("switches directly between tool menus", () => {
    render(
      <ToolDockMenuProvider>
        <MenuProbe id="chatgpt" />
        <MenuProbe id="claude" />
      </ToolDockMenuProvider>,
    );

    fireEvent.click(screen.getByRole("button", { name: "open chatgpt" }));
    expect(screen.getByTestId("chatgpt-state")).toHaveTextContent("chatgpt");

    fireEvent.click(screen.getByRole("button", { name: "open claude" }));
    expect(screen.getByTestId("chatgpt-state")).toHaveTextContent("claude");
  });

  test("does not rerender the page that owns the tool dock", () => {
    const parentRender = vi.fn();
    function Page() {
      parentRender();
      return (
        <ToolDockMenuProvider>
          <MenuProbe id="chatgpt" />
        </ToolDockMenuProvider>
      );
    }

    render(<Page />);
    fireEvent.click(screen.getByRole("button", { name: "open chatgpt" }));

    expect(parentRender).toHaveBeenCalledTimes(1);
    expect(screen.getByTestId("chatgpt-state")).toHaveTextContent("chatgpt");
  });

  test("keeps the existing delayed close behavior", () => {
    vi.useFakeTimers();
    render(
      <ToolDockMenuProvider>
        <MenuProbe id="chatgpt" />
      </ToolDockMenuProvider>,
    );

    fireEvent.click(screen.getByRole("button", { name: "open chatgpt" }));
    fireEvent.click(screen.getByRole("button", { name: "leave chatgpt" }));

    act(() => vi.advanceTimersByTime(TOOL_DOCK_CLOSE_DELAY_MS - 1));
    expect(screen.getByTestId("chatgpt-state")).toHaveTextContent("chatgpt");

    act(() => vi.advanceTimersByTime(1));
    expect(screen.getByTestId("chatgpt-state")).toHaveTextContent("closed");
  });

  test("keeps a clicked menu open until it is clicked again", () => {
    vi.useFakeTimers();
    render(
      <ToolDockMenuProvider>
        <MenuProbe id="chatgpt" />
      </ToolDockMenuProvider>,
    );

    fireEvent.click(screen.getByRole("button", { name: "pin chatgpt" }));
    fireEvent.click(screen.getByRole("button", { name: "leave chatgpt" }));
    act(() => vi.advanceTimersByTime(TOOL_DOCK_CLOSE_DELAY_MS));
    expect(screen.getByTestId("chatgpt-state")).toHaveTextContent("chatgpt");

    fireEvent.click(screen.getByRole("button", { name: "pin chatgpt" }));
    expect(screen.getByTestId("chatgpt-state")).toHaveTextContent("closed");
  });
});
