import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { describe, expect, test } from "vitest";

import { PriceRatioInput } from "./PriceRatioInput";

function Harness() {
  const [value, setValue] = useState(1);
  return (
    <>
      <label htmlFor="ratio">ratio</label>
      <PriceRatioInput id="ratio" value={value} max={10} onValueChange={setValue} />
      <output>{value}</output>
    </>
  );
}

describe("PriceRatioInput", () => {
  test("preserves decimal editing and commits the numeric ratio", async () => {
    const user = userEvent.setup();
    render(<Harness />);
    const input = screen.getByRole("textbox", { name: "ratio" });

    await user.clear(input);
    await user.type(input, "0.85");

    expect(input).toHaveValue("0.85");
    expect(screen.getByText("0.85", { selector: "output" })).toBeInTheDocument();
  });

  test("accepts a localized decimal comma and normalizes it on blur", async () => {
    const user = userEvent.setup();
    render(<Harness />);
    const input = screen.getByRole("textbox", { name: "ratio" });

    await user.clear(input);
    await user.type(input, "1,25");
    await user.tab();

    expect(input).toHaveValue("1.25");
    expect(screen.getByText("1.25", { selector: "output" })).toBeInTheDocument();
  });

  test("does not turn an empty draft into a free offer", async () => {
    const user = userEvent.setup();
    render(<Harness />);
    const input = screen.getByRole("textbox", { name: "ratio" });

    await user.clear(input);
    expect(screen.getByText("1", { selector: "output" })).toBeInTheDocument();
    await user.tab();

    expect(input).toHaveValue("1");
    expect(screen.getByText("1", { selector: "output" })).toBeInTheDocument();
  });

  test("clamps an out-of-range draft when editing finishes", async () => {
    const user = userEvent.setup();
    render(<Harness />);
    const input = screen.getByRole("textbox", { name: "ratio" });

    await user.clear(input);
    await user.type(input, "99");
    expect(input).toHaveValue("99");
    await user.tab();

    expect(input).toHaveValue("10");
    expect(screen.getByText("10", { selector: "output" })).toBeInTheDocument();
  });

  test("rejects negative values while editing", async () => {
    const user = userEvent.setup();
    render(<Harness />);
    const input = screen.getByRole("textbox", { name: "ratio" });

    await user.clear(input);
    await user.type(input, "-1");

    expect(input).toHaveValue("1");
    expect(screen.getByText("1", { selector: "output" })).toBeInTheDocument();
  });
});
