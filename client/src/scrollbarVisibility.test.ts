import { afterEach, describe, expect, it, vi } from "vitest";

import { installAutoHidingScrollbars } from "./scrollbarVisibility";

afterEach(() => {
  vi.useRealTimers();
  document.documentElement.style.removeProperty("--app-scrollbar-opacity");
  delete document.documentElement.dataset.platform;
  document.body.replaceChildren();
});

function horizontalScrollArea(overflowX = "auto", scrollWidth = 400) {
  const element = document.createElement("div");
  element.style.overflowX = overflowX;
  Object.defineProperties(element, {
    clientWidth: { value: 200 },
    scrollWidth: { value: scrollWidth },
  });
  document.body.appendChild(element);
  return element;
}

describe("auto-hiding scrollbars", () => {
  it("reveals the horizontal container from nested content and fades again after pointer activity stops", () => {
    vi.useFakeTimers();
    const scrollArea = horizontalScrollArea();
    const child = document.createElement("span");
    scrollArea.appendChild(child);
    const uninstall = installAutoHidingScrollbars(document);

    child.dispatchEvent(new MouseEvent("pointerover", { bubbles: true }));
    expect(scrollArea.style.getPropertyValue("--app-scrollbar-opacity")).toBe("100%");
    expect(child.style.getPropertyValue("--app-scrollbar-opacity")).toBe("");

    vi.advanceTimersByTime(650);
    vi.advanceTimersToNextFrame();
    vi.advanceTimersToNextFrame();
    const opacity = Number.parseFloat(scrollArea.style.getPropertyValue("--app-scrollbar-opacity"));
    expect(opacity).toBeGreaterThan(0);
    expect(opacity).toBeLessThan(100);

    child.dispatchEvent(new MouseEvent("pointermove", { bubbles: true }));
    expect(scrollArea.style.getPropertyValue("--app-scrollbar-opacity")).toBe("100%");
    vi.advanceTimersByTime(1200);
    expect(scrollArea.style.getPropertyValue("--app-scrollbar-opacity")).toBe("");
    uninstall();
  });

  it("reveals only the nearest horizontally scrollable ancestor, skipping clipped content", () => {
    vi.useFakeTimers();
    const outer = horizontalScrollArea();
    const inner = horizontalScrollArea("scroll");
    const clipped = horizontalScrollArea("hidden");
    outer.appendChild(inner);
    inner.appendChild(clipped);
    const uninstall = installAutoHidingScrollbars(document);

    clipped.dispatchEvent(new MouseEvent("pointerover", { bubbles: true }));
    expect(inner.style.getPropertyValue("--app-scrollbar-opacity")).toBe("100%");
    expect(outer.style.getPropertyValue("--app-scrollbar-opacity")).toBe("");
    expect(clipped.style.getPropertyValue("--app-scrollbar-opacity")).toBe("");
    uninstall();
  });

  it("does not reveal fitted or non-scrollable horizontal surfaces on hover", () => {
    vi.useFakeTimers();
    const surfaces = [horizontalScrollArea("auto", 200), horizontalScrollArea("hidden"), horizontalScrollArea("visible")];
    const uninstall = installAutoHidingScrollbars(document);

    for (const surface of surfaces) {
      surface.dispatchEvent(new MouseEvent("pointerover", { bubbles: true }));
      expect(surface.style.getPropertyValue("--app-scrollbar-opacity")).toBe("");
    }
    uninstall();
  });

  it("holds, gradually fades, and then hides the scrolling surface", () => {
    vi.useFakeTimers();
    document.documentElement.dataset.platform = "windows";
    const scrollArea = document.createElement("div");
    document.body.appendChild(scrollArea);
    const uninstall = installAutoHidingScrollbars(document, {
      holdMs: 650,
      fadeMs: 480,
    });

    scrollArea.dispatchEvent(new Event("scroll"));
    expect(scrollArea.style.getPropertyValue("--app-scrollbar-opacity")).toBe("100%");

    vi.advanceTimersByTime(650);
    vi.advanceTimersToNextFrame();
    vi.advanceTimersToNextFrame();
    const fadingOpacity = Number.parseFloat(
      scrollArea.style.getPropertyValue("--app-scrollbar-opacity"),
    );
    expect(fadingOpacity).toBeGreaterThan(0);
    expect(fadingOpacity).toBeLessThan(100);

    vi.advanceTimersByTime(480);
    expect(scrollArea.style.getPropertyValue("--app-scrollbar-opacity")).toBe("");
    uninstall();
  });

  it("returns to fully visible when scrolling resumes during the fade", () => {
    vi.useFakeTimers();
    document.documentElement.dataset.platform = "linux";
    const scrollArea = document.createElement("div");
    document.body.appendChild(scrollArea);
    const uninstall = installAutoHidingScrollbars(document, {
      holdMs: 650,
      fadeMs: 480,
    });

    scrollArea.dispatchEvent(new Event("scroll"));
    vi.advanceTimersByTime(650);
    vi.advanceTimersToNextFrame();
    vi.advanceTimersToNextFrame();
    scrollArea.dispatchEvent(new Event("scroll"));

    expect(scrollArea.style.getPropertyValue("--app-scrollbar-opacity")).toBe("100%");
    vi.advanceTimersByTime(649);
    expect(scrollArea.style.getPropertyValue("--app-scrollbar-opacity")).toBe("100%");
    uninstall();
  });

  it("leaves macOS scrollbar visibility to the system", () => {
    vi.useFakeTimers();
    document.documentElement.dataset.platform = "macos";
    const scrollArea = horizontalScrollArea();
    const uninstall = installAutoHidingScrollbars(document);

    scrollArea.dispatchEvent(new Event("scroll"));
    scrollArea.dispatchEvent(new MouseEvent("pointerover", { bubbles: true }));
    expect(scrollArea.style.getPropertyValue("--app-scrollbar-opacity")).toBe("");
    uninstall();
  });

  it("removes inline state and the listener during cleanup", () => {
    vi.useFakeTimers();
    document.documentElement.dataset.platform = "windows";
    const scrollArea = horizontalScrollArea();
    const uninstall = installAutoHidingScrollbars(document);

    scrollArea.dispatchEvent(new Event("scroll"));
    uninstall();
    expect(scrollArea.style.getPropertyValue("--app-scrollbar-opacity")).toBe("");

    scrollArea.dispatchEvent(new Event("scroll"));
    scrollArea.dispatchEvent(new MouseEvent("pointermove", { bubbles: true }));
    expect(scrollArea.style.getPropertyValue("--app-scrollbar-opacity")).toBe("");
  });
});
