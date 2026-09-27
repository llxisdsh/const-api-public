import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, test, vi } from "vitest";

import type { ChannelSurfaceBinding } from "../appTypes";
import { changeAppLanguage } from "../i18n";
import { SupplierSurfaceConfig } from "./SupplierSurfaceConfig";

const verification = { state: "declared", checked_at_unix: 0, summary: "" };
const rows: ChannelSurfaceBinding[] = [
  {
    surface: "open_ai",
    base_url: "https://gateway.example.test/v1",
    endpoint_profile: "openai_compatible",
    auth_scheme: "bearer",
    protocols: [{ protocol: "openai_chat", preferred: true, verification }],
    operation_overrides: [],
    verification,
  },
  {
    surface: "anthropic",
    base_url: "",
    endpoint_profile: "anthropic_compatible",
    auth_scheme: "x_api_key",
    protocols: [],
    operation_overrides: [],
    verification,
  },
  {
    surface: "gemini",
    base_url: "",
    endpoint_profile: "gemini_compatible",
    auth_scheme: "x_goog_api_key",
    protocols: [],
    operation_overrides: [],
    verification,
  },
];

describe("SupplierSurfaceConfig", () => {
  beforeEach(async () => {
    await changeAppLanguage("zh-CN");
  });

  test("shows every custom API interface and enables one through the shared handler", async () => {
    const onToggleProtocol = vi.fn();
    render(
      <SupplierSurfaceConfig
        mode="custom-edit"
        bindings={[rows[0]]}
        rows={rows}
        defaultTarget={{ surface: "open_ai", protocol: "openai_chat" }}
        onToggleProtocol={onToggleProtocol}
      />,
    );

    expect(screen.getByText("支持的 API 接口")).toBeInTheDocument();
    expect(screen.getByText("OpenAI API")).toBeInTheDocument();
    expect(screen.getByText("Anthropic API")).toBeInTheDocument();
    expect(screen.queryAllByText("Base URL")).toHaveLength(1);

    await userEvent.click(screen.getByRole("checkbox", { name: "Anthropic Messages" }));
    expect(onToggleProtocol).toHaveBeenCalledWith(
      "anthropic",
      "anthropic_messages",
      true,
    );
  });

  test("keeps driver-declared interfaces read-only", () => {
    render(<SupplierSurfaceConfig mode="readonly" bindings={[rows[0]]} />);

    expect(screen.queryByRole("checkbox")).not.toBeInTheDocument();
    expect(screen.getByText("OpenAI Chat")).toBeInTheDocument();
    expect(screen.getByText("https://gateway.example.test/v1")).toBeInTheDocument();
  });
});
