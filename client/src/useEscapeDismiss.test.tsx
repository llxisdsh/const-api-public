// @vitest-environment jsdom

import { fireEvent, render } from "@testing-library/react";
import { describe, expect, test, vi } from "vitest";

import { useEscapeDismiss } from "./useEscapeDismiss";

function EscapeTarget({
  label,
  onDismiss,
}: {
  label: string;
  onDismiss: () => void;
}) {
  useEscapeDismiss(onDismiss);
  return <div>{label}</div>;
}

describe("useEscapeDismiss", () => {
  test("dismisses only the topmost registered surface", () => {
    const dismissOuter = vi.fn();
    const dismissInner = vi.fn();
    const { rerender } = render(
      <>
        <EscapeTarget label="outer" onDismiss={dismissOuter} />
        <EscapeTarget label="inner" onDismiss={dismissInner} />
      </>,
    );

    fireEvent.keyDown(document, { key: "Escape" });
    expect(dismissInner).toHaveBeenCalledTimes(1);
    expect(dismissOuter).not.toHaveBeenCalled();

    rerender(<EscapeTarget label="outer" onDismiss={dismissOuter} />);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(dismissOuter).toHaveBeenCalledTimes(1);
  });

  test("leaves Escape to a focused control that already consumed it", () => {
    const onDismiss = vi.fn();

    function ConsumingTarget() {
      useEscapeDismiss(onDismiss);
      return (
        <button type="button" onKeyDown={(event) => event.preventDefault()}>
          menu
        </button>
      );
    }

    const { getByRole } = render(<ConsumingTarget />);
    fireEvent.keyDown(getByRole("button"), { key: "Escape" });
    expect(onDismiss).not.toHaveBeenCalled();
  });
});
