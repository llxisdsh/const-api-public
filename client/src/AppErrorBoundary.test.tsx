import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { AppErrorBoundary, RendererFailure } from "./AppErrorBoundary";

function BrokenView(): never {
  throw new Error("render boom");
}

describe("AppErrorBoundary", () => {
  it("replaces a failed renderer tree with the reload fallback", () => {
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => undefined);
    const reload = vi.fn();

    render(
      <AppErrorBoundary fallback={<RendererFailure onReload={reload} />}>
        <BrokenView />
      </AppErrorBoundary>,
    );

    expect(screen.getByRole("alert")).toHaveTextContent("界面出现异常");
    fireEvent.click(screen.getByRole("button", { name: "重新加载" }));
    expect(reload).toHaveBeenCalledOnce();
    expect(consoleError).toHaveBeenCalled();
    consoleError.mockRestore();
  });
});
