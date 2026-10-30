import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useEffect, useState } from "react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { applyLocalPriorities } from "../channelOrdering";
import { ChannelPriorityTable } from "./ChannelPriorityTable";

const initial = [
  { id: "a", name: "Account A", local_priority: 1 },
  { id: "b", name: "Account B", local_priority: 1 },
  { id: "c", name: "Backup", local_priority: 3 },
];
const onOpen = vi.fn();
beforeEach(() => {
  vi.stubGlobal("PointerEvent", class extends MouseEvent {
    pointerId: number;
    constructor(type: string, options: PointerEventInit = {}) { super(type, options); this.pointerId = options.pointerId ?? 1; }
  });
  Object.defineProperty(HTMLElement.prototype, "setPointerCapture", { configurable: true, value: vi.fn() });
  Object.defineProperty(HTMLElement.prototype, "animate", { configurable: true, value: vi.fn(() => ({ cancel: vi.fn() })) });
  vi.stubGlobal("matchMedia", vi.fn(() => ({ matches: false })));
  vi.spyOn(HTMLElement.prototype, "offsetTop", "get").mockImplementation(function (this: HTMLElement) {
    return this.getBoundingClientRect().top - 64;
  });
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
    const row = this.closest<HTMLElement>("[data-priority-channel]");
    const index = row ? [...row.parentElement!.querySelectorAll("[data-priority-channel]")].indexOf(row) : -1;
    const top = index >= 0 ? 100 + index * 60 : 64;
    if (this.classList.contains("supplier-channel-priority")) {
      return { x: 500, y: top + 14, left: 500, right: 552, top: top + 14, bottom: top + 46,
        width: 52, height: 32, toJSON: () => ({}) };
    }
    return { x: 0, y: top, left: 0, right: 800, top, bottom: index >= 0 ? top + 60 : 280,
      width: 800, height: index >= 0 ? 60 : 216, toJSON: () => ({}) };
  });
  onOpen.mockClear();
});
afterEach(() => {
  vi.restoreAllMocks(); vi.unstubAllGlobals();
  Reflect.deleteProperty(HTMLElement.prototype, "setPointerCapture");
  Reflect.deleteProperty(HTMLElement.prototype, "animate");
});

function Harness({ save = vi.fn().mockResolvedValue(true), disabled = false, items = initial }: {
  save?: (values: Readonly<Record<string, number>>) => Promise<boolean>; disabled?: boolean; items?: typeof initial;
}) {
  const [channels, setChannels] = useState(items);
  useEffect(() => setChannels(items), [items]);
  return <ChannelPriorityTable channels={channels} allChannels={channels} disabled={disabled}
    selectedId="a" header={<span>Channels</span>} empty="Empty" name={(channel) => channel.name}
    onOpen={onOpen} onSave={async (values) => {
      const ok = await save(values);
      if (ok) setChannels((current) => applyLocalPriorities(current, values));
      return ok;
    }}>
    {(channel, handle) => <><span>{channel.name}</span><span data-testid={`value-${channel.id}`}>{handle}</span></>}
  </ChannelPriorityTable>;
}
function handle(name: string) { return screen.getByRole("button", { name: new RegExp(`^${name} 的优先级 \\d+，拖动调整$`) }); }
function begin(name: string, y: number) {
  fireEvent.pointerDown(handle(name), { pointerId: 1, button: 0, clientX: 526, clientY: y });
}
function move(y: number, x = 526) { fireEvent.pointerMove(window, { pointerId: 1, clientX: x, clientY: y }); }
function drop(y: number, x = 526) { fireEvent.pointerUp(window, { pointerId: 1, clientX: x, clientY: y }); }

describe("inline channel priority drag", () => {
  test("pressing lifts immediately, but click jitter does not start dragging or save", () => {
    const save = vi.fn().mockResolvedValue(true);
    const { container } = render(<Harness save={save} />);
    begin("Backup", 250);
    expect(container.querySelector(".priority-drag-source")).not.toBeNull();
    expect(document.querySelector(".supplier-priority-drag-preview")).toHaveTextContent("Backup3");
    const overlay = document.querySelector<HTMLElement>(".supplier-priority-drag-preview")!;
    expect(overlay.style.left).toBe("0px");
    expect(overlay.style.top).toBe("217px");
    move(252, 528);
    expect(overlay.style.top).toBe("217px");
    expect(container.querySelector(".priority-drop-join")).toBeNull();
    expect(handle("Backup").querySelector("svg")).toBeNull();
    expect(save).not.toHaveBeenCalled();
    drop(252, 528);
    expect(document.querySelector(".supplier-priority-drag-preview")).toBeNull();
    expect(handle("Backup")).toHaveTextContent("3");
    expect(save).not.toHaveBeenCalled();
  });
  test("highlights a whole target group, saves on drop, and can undo", async () => {
    const save = vi.fn().mockResolvedValue(true);
    const { container } = render(<Harness save={save} />);
    begin("Backup", 250); move(130);
    expect(container.querySelectorAll(".priority-drop-join:not(.priority-drag-source)")).toHaveLength(2);
    expect(handle("Account A")).toHaveClass("priority-number-target");
    expect(handle("Account B")).not.toHaveClass("priority-number-target");
    expect(handle("Backup")).toHaveTextContent("1");
    expect(screen.getByRole("status")).toBeEmptyDOMElement();
    expect(container.querySelector(".priority-drag-source")).not.toBeNull();
    expect(document.querySelector(".supplier-priority-drag-preview")).toHaveTextContent("Backup1");
    expect(document.querySelector(".supplier-priority-drag-preview .priority-number-target")).toHaveTextContent("1");
    expect(save).not.toHaveBeenCalled();
    drop(130);
    fireEvent.click(handle("Backup"));
    expect(onOpen).not.toHaveBeenCalled();
    await waitFor(() => expect(screen.getByTestId("value-c")).toHaveTextContent("1"));
    expect(save).toHaveBeenCalledExactlyOnceWith({ c: 1 });
    fireEvent.click(screen.getByRole("button", { name: "撤销" }));
    await waitFor(() => expect(screen.getByTestId("value-c")).toHaveTextContent("3"));
    expect(save).toHaveBeenLastCalledWith({ c: 3 });
  });
  test.each([[499, 130], [553, 130]])(
    "outside the number box at %s,%s inserts rather than merging", async (x, y) => {
      const save = vi.fn().mockResolvedValue(true);
      const { container } = render(<Harness save={save} />);
      begin("Backup", 250); move(y, x);
      expect(container.querySelector(".priority-drop-join")).toBeNull();
      expect(container.querySelector("[data-priority-channel]")).toHaveAttribute("data-priority-channel", "c");
      expect(handle("Account A")).toHaveTextContent("2");
      expect(save).not.toHaveBeenCalled();
      drop(y, x);
      await waitFor(() => expect(save).toHaveBeenCalledExactlyOnceWith({ c: 1, a: 2, b: 2 }));
    },
  );
  test("approaching a number vertically does not push it away before merging", () => {
    const save = vi.fn().mockResolvedValue(true);
    const { container } = render(<Harness save={save} />);
    begin("Backup", 250); move(147);
    expect(container.querySelector(".priority-drop-join")).toBeNull();
    expect(container.querySelector("[data-priority-channel]")).toHaveAttribute("data-priority-channel", "a");
    expect(handle("Account A")).toHaveTextContent("1");
    move(130);
    expect(handle("Account A")).toHaveClass("priority-number-target");
    expect(handle("Backup")).toHaveTextContent("1");
    expect(save).not.toHaveBeenCalled();
  });
  test("crossing into and out of a number switches preview without saving", () => {
    const save = vi.fn().mockResolvedValue(true);
    const { container } = render(<Harness save={save} />);
    begin("Backup", 250); move(130, 180);
    expect(handle("Account A")).toHaveTextContent("2");
    move(190);
    expect(handle("Account A")).toHaveTextContent("1");
    expect(handle("Account A")).toHaveClass("priority-number-target");
    move(170, 180);
    expect(container.querySelector(".priority-drop-join")).toBeNull();
    expect(document.querySelector(".supplier-priority-drag-preview .priority-number-target")).toBeNull();
    expect(container.querySelector("[data-priority-channel]")).toHaveAttribute("data-priority-channel", "c");
    expect(handle("Backup")).toHaveTextContent("1");
    expect(handle("Account A")).toHaveTextContent("2");
    expect(save).not.toHaveBeenCalled();
  });
  test.each([
    { direction: "above", x: 526, y: 113 },
    { direction: "below", x: 526, y: 147 },
    { direction: "left", x: 499, y: 130 },
    { direction: "right", x: 553, y: 130 },
  ])("leaving the number $direction immediately separates the preview and can rejoin", ({ x, y }) => {
    const save = vi.fn().mockResolvedValue(true);
    const { container } = render(<Harness save={save} items={initial.map((channel, index) => ({ ...channel, local_priority: index + 1 }))} />);
    begin("Backup", 250); move(130);
    expect(handle("Account A")).toHaveClass("priority-number-target");
    move(y, x);
    expect(container.querySelector(".priority-drop-join")).toBeNull();
    expect(document.querySelector(".supplier-priority-drag-preview .priority-number-target")).toBeNull();
    expect(new Set(["Account A", "Account B", "Backup"].map(name => handle(name).textContent)).size).toBe(3);
    expect(save).not.toHaveBeenCalled();
    const number = handle("Account A").getBoundingClientRect();
    move(number.top + number.height / 2, number.left + number.width / 2);
    expect(handle("Account A")).toHaveClass("priority-number-target");
    expect(handle("Backup").textContent).toBe(handle("Account A").textContent);
  });
  test("entering the empty slot after merging does not retain the hidden number as a target", () => {
    const { container } = render(<Harness items={initial.map((channel, index) => ({ ...channel, local_priority: index + 1 }))} />);
    begin("Backup", 250); move(130);
    move(200);
    expect(container.querySelector(".priority-drop-join")).toBeNull();
    expect(handle("Account A")).toHaveTextContent("1");
    expect(handle("Backup")).toHaveTextContent("2");
    expect(handle("Account B")).toHaveTextContent("3");
  });
  test("release outside a number cannot commit a stale merge without a final pointermove", async () => {
    const save = vi.fn().mockResolvedValue(true);
    render(<Harness save={save} items={initial.map((channel, index) => ({ ...channel, local_priority: index + 1 }))} />);
    begin("Backup", 250); move(130);
    drop(147);
    await waitFor(() => expect(save).toHaveBeenCalledExactlyOnceWith({ c: 2, b: 3 }));
    expect(handle("Account A")).toHaveTextContent("1");
    expect(handle("Backup")).toHaveTextContent("2");
    expect(handle("Account B")).toHaveTextContent("3");
  });
  test("rows make room before release and the open slot keeps the preview stable", async () => {
    const save = vi.fn().mockResolvedValue(true);
    const { container } = render(<Harness save={save} />);
    const order = () => [...container.querySelectorAll<HTMLElement>("[data-priority-channel]")].map(row => row.dataset.priorityChannel);
    begin("Backup", 250); move(130, 180);
    expect(order()).toEqual(["c", "a", "b"]);
    expect(save).not.toHaveBeenCalled();
    move(132, 180);
    expect(order()).toEqual(["c", "a", "b"]);
    expect(handle("Account A")).toHaveTextContent("2");
    drop(132, 180);
    await waitFor(() => expect(save).toHaveBeenCalledExactlyOnceWith({ c: 1, a: 2, b: 2 }));
    expect(order()).toEqual(["c", "a", "b"]);
  });
  test("joining moves the slot into its group before saving", () => {
    const save = vi.fn().mockResolvedValue(true);
    const { container } = render(<Harness save={save} items={initial.map((channel, index) => ({ ...channel, local_priority: index + 1 }))} />);
    begin("Backup", 250); move(130);
    expect([...container.querySelectorAll<HTMLElement>("[data-priority-channel]")].map(row => row.dataset.priorityChannel)).toEqual(["a", "c", "b"]);
    expect(container.querySelector('[data-priority-channel="c"]')).toHaveClass("priority-same-group");
    expect(save).not.toHaveBeenCalled();
    fireEvent.keyDown(window, { key: "Escape" });
    expect([...container.querySelectorAll<HTMLElement>("[data-priority-channel]")].map(row => row.dataset.priorityChannel)).toEqual(["a", "b", "c"]);
    expect(handle("Backup")).toHaveTextContent("3");
  });
  test("moving rows do not flip an insertion, but dragging back can restore the original order", () => {
    const save = vi.fn().mockResolvedValue(true);
    const { container } = render(<Harness save={save} />);
    const order = () => [...container.querySelectorAll<HTMLElement>("[data-priority-channel]")].map(row => row.dataset.priorityChannel);
    begin("Backup", 250); move(217, 180);
    expect(order()).toEqual(["c", "a", "b"]);
    move(218, 180);
    expect(order()).toEqual(["c", "a", "b"]);
    move(270, 180);
    expect(order()).toEqual(["a", "b", "c"]);
    drop(270, 180);
    expect(save).not.toHaveBeenCalled();
  });
  test("moving a grouped channel out of its number previews a split and can rejoin", () => {
    const save = vi.fn().mockResolvedValue(true);
    const { container } = render(<Harness save={save} />);
    begin("Account B", 190); move(191, 180);
    expect(handle("Account B")).toHaveTextContent("2");
    expect(container.querySelector('[data-priority-channel="b"]')).not.toHaveClass("priority-same-group");
    move(130);
    expect(handle("Account B")).toHaveTextContent("1");
    expect(container.querySelector('[data-priority-channel="b"]')).toHaveClass("priority-same-group");
    expect(save).not.toHaveBeenCalled();
  });
  test.each([
    { direction: "up", source: "Backup", start: 250, end: 217, changes: { c: 1, a: 2, b: 2 }, order: ["c", "a", "b"] },
    { direction: "down", source: "Account A", start: 130, end: 223, changes: { c: 2, a: 3 }, order: ["b", "c", "a"] },
  ])("moving $direction reorders on entering a peer row, before crossing its midpoint", async ({ source, start, end, changes, order }) => {
    const save = vi.fn().mockResolvedValue(true);
    const { container } = render(<Harness save={save} />);
    begin(source, start); move(end, 180);
    expect(container.querySelector(".priority-drop-join")).toBeNull();
    expect(handle(source)).toHaveTextContent(String(changes[source === "Backup" ? "c" : "a"]));
    expect(save).not.toHaveBeenCalled();
    drop(end, 180);
    await waitFor(() => expect(save).toHaveBeenCalledExactlyOnceWith(changes));
    expect([...container.querySelectorAll<HTMLElement>("[data-priority-channel]")].map((row) => row.dataset.priorityChannel)).toEqual(order);
  });
  test("inserting ahead of a peer ignores the lifted row's empty slot", async () => {
    const save = vi.fn().mockResolvedValue(true);
    const { container } = render(<Harness save={save} />);
    begin("Account A", 130); move(170, 180);
    expect(container.querySelector("[data-priority-channel]")).toHaveAttribute("data-priority-channel", "a");
    expect(handle("Account B")).toHaveTextContent("2");
    drop(170, 180);
    await waitFor(() => expect(save).toHaveBeenCalledExactlyOnceWith({ b: 2 }));
  });
  test("an edge inserts a separate tier instead of joining its row", async () => {
    const save = vi.fn().mockResolvedValue(true);
    const { container } = render(<Harness save={save} />);
    begin("Backup", 250); move(102, 180);
    expect(container.querySelector("[data-priority-channel]")).toHaveAttribute("data-priority-channel", "c");
    expect(container.querySelector(".priority-drop-join")).toBeNull();
    expect(handle("Account A")).toHaveTextContent("2");
    expect(handle("Account B")).toHaveTextContent("2");
    expect(handle("Backup")).toHaveTextContent("1");
    expect(save).not.toHaveBeenCalled();
    drop(102, 180);
    await waitFor(() => expect(save).toHaveBeenCalledExactlyOnceWith({ c: 1, a: 2, b: 2 }));
  });
  test.each([
    { edge: "top", source: "Backup", start: 250, near: 104, far: -60, order: ["c", "a", "b"], changes: { c: 1, a: 2, b: 2 } },
    { edge: "bottom", source: "Account A", start: 130, near: 276, far: 480, order: ["b", "c", "a"], changes: { c: 2, a: 3 } },
  ])("the $edge edge stays valid beyond the table and saves the preview on release", async ({ source, start, near, far, order, changes }) => {
    const save = vi.fn().mockResolvedValue(true);
    const { container } = render(<Harness save={save} />);
    const ids = () => [...container.querySelectorAll<HTMLElement>("[data-priority-channel]")].map(row => row.dataset.priorityChannel);
    begin(source, start); move(near);
    expect(ids()).toEqual(order);
    expect(container.querySelector(".priority-drop-join")).toBeNull();
    move(far);
    expect(ids()).toEqual(order);
    expect(save).not.toHaveBeenCalled();
    drop(far);
    await waitFor(() => expect(save).toHaveBeenCalledExactlyOnceWith(changes));
    expect(ids()).toEqual(order);
  });
  test("moving directly above the header can then return to a number and merge", () => {
    const save = vi.fn().mockResolvedValue(true);
    const { container } = render(<Harness save={save} />);
    begin("Backup", 250); move(20);
    expect(container.querySelector("[data-priority-channel]")).toHaveAttribute("data-priority-channel", "c");
    expect(handle("Account A")).toHaveTextContent("2");
    move(190);
    expect(handle("Account A")).toHaveClass("priority-number-target");
    expect(handle("Account A")).toHaveTextContent("1");
    expect(save).not.toHaveBeenCalled();
  });
  test("same-group moves, Escape, and horizontal outside drops do not save", () => {
    const save = vi.fn().mockResolvedValue(true);
    render(<Harness save={save} />);
    begin("Account B", 190); move(130); drop(130);
    begin("Backup", 250); move(130); fireEvent.keyDown(window, { key: "Escape" }); drop(130);
    expect(handle("Backup")).toHaveTextContent("3");
    begin("Backup", 250); move(130, 900); drop(130, 900);
    expect(handle("Backup")).toHaveTextContent("3");
    expect(document.querySelector(".supplier-priority-drag-preview")).toBeNull();
    expect(save).not.toHaveBeenCalled();
  });
  test("the lifted row follows the pointer and previews its priority without extra tips", () => {
    render(<Harness />);
    begin("Backup", 250); move(130);
    const overlay = document.querySelector<HTMLElement>(".supplier-priority-drag-preview")!;
    const top = overlay.style.top;
    expect(overlay).toHaveAttribute("aria-hidden", "true");
    expect(overlay).toHaveAttribute("inert");
    move(190);
    expect(overlay.style.top).not.toBe(top);
    expect(overlay.textContent).toBe("Backup1");
  });
  test.each(["blur", "pointercancel"])("%s cancels dragging without swallowing the next click", (event) => {
    const save = vi.fn().mockResolvedValue(true);
    const { container } = render(<Harness save={save} />);
    begin("Backup", 250); move(130);
    fireEvent(window, new Event(event));
    expect(container.querySelector(".priority-dragging")).toBeNull();
    fireEvent.click(screen.getByText("Account B"));
    expect(onOpen).toHaveBeenCalledExactlyOnceWith("b");
    expect(save).not.toHaveBeenCalled();
  });
  test("name refreshes retain a drag, but concurrent priority changes cancel it", () => {
    const save = vi.fn().mockResolvedValue(true);
    const view = render(<Harness save={save} />);
    begin("Backup", 250); move(130);
    view.rerender(<Harness save={save} items={initial.map((item) => ({ ...item, name: `${item.name} refreshed` }))} />);
    expect(view.container.querySelectorAll(".priority-drop-join:not(.priority-drag-source)")).toHaveLength(2);
    view.rerender(<Harness save={save} items={initial.map((item) => ({ ...item, local_priority: item.local_priority + 1 }))} />);
    expect(view.container.querySelector(".priority-dragging")).toBeNull();
    drop(130);
    expect(save).not.toHaveBeenCalled();
  });
  test("undo expires when an affected priority is edited elsewhere", async () => {
    const view = render(<Harness />);
    begin("Backup", 250); move(130); drop(130);
    expect(handle("Backup")).toHaveTextContent("1");
    await screen.findByRole("button", { name: "撤销" });
    view.rerender(<Harness items={initial.map((item) => item.id === "c" ? { ...item, local_priority: 9 } : item)} />);
    expect(screen.getByTestId("value-c")).toHaveTextContent("9");
    expect(screen.queryByRole("button", { name: "撤销" })).toBeNull();
  });
  test("failed saves keep the old priorities and pending saves block another move", async () => {
    let finish!: (ok: boolean) => void;
    const save = vi.fn(() => new Promise<boolean>((resolve) => { finish = resolve; }));
    render(<Harness save={save} />);
    begin("Backup", 250); move(130); drop(130);
    expect(handle("Backup")).toHaveTextContent("1");
    expect(handle("Account A")).toBeDisabled();
    begin("Account B", 190); move(278); drop(278);
    expect(save).toHaveBeenCalledTimes(1);
    await act(async () => finish(false));
    expect(screen.getByTestId("value-c")).toHaveTextContent("3");
    expect(screen.getByRole("status")).toHaveTextContent("未能保存");
    expect(screen.queryByRole("button", { name: "撤销" })).toBeNull();
  });
  test("clicking a priority does not open anything; disabled state prevents dragging", () => {
    const save = vi.fn().mockResolvedValue(true);
    const view = render(<Harness save={save} />);
    fireEvent.click(handle("Account A"));
    expect(onOpen).not.toHaveBeenCalled();
    expect(save).not.toHaveBeenCalled();
    fireEvent.click(screen.getByText("Account B"));
    expect(onOpen).toHaveBeenCalledExactlyOnceWith("b");
    view.rerender(<Harness save={save} disabled />);
    begin("Backup", 250); move(130); drop(130);
    expect(save).not.toHaveBeenCalled();
  });
  test("keyboard changes preview inline and only Enter saves", async () => {
    const save = vi.fn().mockResolvedValue(true);
    render(<Harness save={save} />);
    fireEvent.keyDown(handle("Backup"), { key: "ArrowUp" });
    expect(handle("Backup")).toHaveTextContent("2");
    expect(save).not.toHaveBeenCalled();
    fireEvent.keyDown(handle("Backup"), { key: "Escape" });
    expect(handle("Backup")).toHaveTextContent("3");
    fireEvent.keyDown(handle("Backup"), { key: "ArrowDown" });
    fireEvent.blur(handle("Backup"));
    expect(handle("Backup")).toHaveTextContent("3");
    fireEvent.keyDown(handle("Backup"), { key: "ArrowUp" });
    fireEvent.keyDown(handle("Backup"), { key: "Enter" });
    await waitFor(() => expect(save).toHaveBeenCalledExactlyOnceWith({ c: 2 }));
  });
});
