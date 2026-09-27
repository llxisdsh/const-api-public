import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { DialogBackdrop } from "./DialogBackdrop";

describe("DialogBackdrop", () => {
  it("marks modal and drawer backdrops as explicitly non-dismissible", () => {
    const { rerender } = render(
      <DialogBackdrop aria-label="Modal">
        <div>Modal content</div>
      </DialogBackdrop>,
    );

    expect(screen.getByRole("dialog", { name: "Modal" })).toHaveClass("modal-backdrop");
    expect(screen.getByRole("dialog", { name: "Modal" })).toHaveAttribute(
      "data-backdrop-dismissible",
      "false",
    );

    rerender(
      <DialogBackdrop aria-label="Drawer" variant="drawer">
        <div>Drawer content</div>
      </DialogBackdrop>,
    );

    expect(screen.getByRole("dialog", { name: "Drawer" })).toHaveClass("drawer-backdrop");
    expect(screen.getByRole("dialog", { name: "Drawer" })).toHaveAttribute(
      "data-backdrop-dismissible",
      "false",
    );
  });
});
