import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { AvailabilityGroupChecks } from "./AvailabilityGroupChecks";
import type { AvailabilityGroupResult } from "../sourceDrivers";

const runtimeGroup = (group: string, status = "available"): AvailabilityGroupResult => ({
  group, status, representative: "gpt-luna", upstream_model: "openai/gpt-luna",
  attempts: 1, checked_at: 1789110732, next_probe_at: 0,
  cause: "", rule_id: "", message: "", scope_inferred: false,
});

describe("AvailabilityGroupChecks", () => {
  it("keeps the result area visible without inventing checks", () => {
    const { container } = render(<AvailabilityGroupChecks checks={[]} />);
    expect(screen.getByText("每组测试一个模型，并检查协议能力")).toBeInTheDocument();
    expect(screen.getByText("尚未完整检查")).toBeInTheDocument();
    expect(container.querySelectorAll("details")).toHaveLength(0);
    expect(container.querySelector("time")).toBeNull();
  });
  it("explains inferred channel admission and exposes the representative detail", () => {
    render(<AvailabilityGroupChecks checks={[
      { name: "availability_group", capability: "other", status: "suspended", message: "qwen-27b: HTTP 503; 2 attempts" },
      { name: "availability_group", capability: "openai_closed", status: "available", message: "gpt-luna: 1 attempt" },
    ]} />);
    expect(screen.getByText("其他模型组检查未通过，渠道已暂停供应。")).toBeInTheDocument();
    const summary = screen.getByRole("button", { name: /^每组测试一个模型/ });
    fireEvent.click(summary);
    expect(summary).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByText("qwen-27b: HTTP 503; 2 attempts")).toBeVisible();
    expect(screen.getByText("通过")).toBeInTheDocument();
  });
  it("treats future states as inconclusive, not failure", () => {
    render(<AvailabilityGroupChecks checks={[{ name: "availability_group", capability: "future", status: "future" }]} />);
    expect(screen.getByText("尚未确认")).toBeInTheDocument();
    expect(screen.queryByText("暂停供应")).not.toBeInTheDocument();
  });

  it("shows current runtime results even when config evidence is empty", () => {
    const group = runtimeGroup("openai_closed");
    group.representative = "openai/gpt-luna";
    const { container } = render(<AvailabilityGroupChecks checks={[]} groups={[group]} />);
    expect(screen.getByText("OpenAI 闭源模型")).toBeInTheDocument();
    expect(screen.queryByText("尚未完整检查")).not.toBeInTheDocument();
    expect(container.querySelector("time")).toHaveAttribute("datetime", new Date(group.checked_at * 1000).toISOString());
    const summary = screen.getByRole("button", { name: /^每组测试一个模型/ });
    expect(summary).toContainElement(container.querySelector("time"));
    expect(summary).not.toHaveTextContent(/通过|暂停供应|最近检查|（/);
    expect(container.querySelector("time")).toHaveAttribute("title", expect.stringContaining("最近检查:"));
    expect(summary).toHaveAttribute("aria-expanded", "false");
    const details = document.getElementById(summary.getAttribute("aria-controls")!);
    expect(details).not.toBeVisible();
    fireEvent.click(summary);
    expect(screen.getByText("测试模型: gpt-luna")).toBeVisible();
    expect(screen.getByText("请求 1 次")).toBeVisible();
    expect(screen.queryByText(/openai\/gpt-luna/)).not.toBeInTheDocument();
    expect(details).toBeVisible();
    fireEvent.click(summary);
    expect(details).not.toBeVisible();
  });

  it("does not resurrect stale config results when the runtime snapshot was invalidated", () => {
    render(<AvailabilityGroupChecks groups={[]} checks={[
      { name: "availability_group", capability: "other", status: "suspended" },
    ]} />);
    expect(screen.getByText("尚未完整检查")).toBeInTheDocument();
    expect(screen.queryByText("暂停供应")).not.toBeInTheDocument();
  });

  it("uses effective runtime conclusions instead of a later inconclusive config attempt", () => {
    render(<AvailabilityGroupChecks groups={[runtimeGroup("google_closed", "suspended")]} checks={[
      { name: "availability_group", capability: "google_closed", status: "inconclusive" },
    ]} />);
    expect(screen.getByText("暂停供应")).toBeInTheDocument();
    expect(screen.queryByText("尚未确认")).not.toBeInTheDocument();
  });

  it("sorts existing groups consistently and keeps only the latest duplicate", () => {
    const { container } = render(<AvailabilityGroupChecks checks={[
      { name: "availability_group", capability: "other", status: "available", checked_at_unix: 2 },
      { name: "availability_group", capability: "openai_closed", status: "available" },
      { name: "availability_group", capability: "anthropic_closed", status: "available" },
      { name: "availability_group", capability: "other", status: "suspended", checked_at_unix: 1 },
      { name: "unrelated", capability: "google_closed", status: "available" },
    ]} />);
    expect([...container.querySelectorAll(".availability-group-item strong")].map((node) => node.textContent))
      .toEqual(["Anthropic 闭源模型", "OpenAI 闭源模型", "其他模型"]);
    expect(screen.queryByText("暂停供应")).not.toBeInTheDocument();
    expect(container.querySelector(".availability-group-list")).toBeInTheDocument();
    expect(container.querySelector(".capability-list")).toBeNull();
  });

  it("ignores missing or invalid dates from old/future evidence", () => {
    const { container } = render(<AvailabilityGroupChecks checks={[
      { name: "availability_group", capability: "other", status: "available", checked_at_unix: Number.MAX_VALUE },
    ]} />);
    expect(screen.getByText("通过")).toBeInTheDocument();
    expect(container.querySelector("time")).toBeNull();
    expect(screen.queryByText(/Invalid Date/)).not.toBeInTheDocument();
  });
});
