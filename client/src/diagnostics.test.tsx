import { act, render, renderHook, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { buildRuntimeDiagnostics, channelFromSupplier, emptyConfig, supplierChannelDiagnostic } from "./appHelpers";
import type { ChannelConfig, ClientConfig, SupplierChannelHealth, SupplierStatus } from "./appTypes";
import { DiagnosticNotice, useDiagnostics } from "./diagnostics";

const channel: ChannelConfig = {
  ...channelFromSupplier("channel-a", emptyConfig.supplier),
  enabled: true,
  share_enabled: true,
};
const config: ClientConfig = { ...emptyConfig, channels: [channel] };
const proxy = { running: true, listen: config.listen };
const offline: SupplierStatus = {
  running: false,
  starting: false,
  node_id: "",
  server_ws_url: "",
  server_quic_url: "",
  transport_preference: "auto",
  connected_transport: "",
  active_transport: "",
  last_error: "no channel passed supplier validation",
};
const health: SupplierChannelHealth = {
  channel_id: channel.id,
  name: channel.name,
  status: "unavailable",
  model: "test-model",
  model_count: 1,
  latency_ms: 0,
  credential_status: "available",
  quota_status: "available",
  message: "upstream connection failed",
  checked_at_unix: 100,
};
const transport = {
  channel_id: channel.id,
  supplier_unit_id: "unit-a",
  connected_transport: "",
  active_transport: "reconnecting",
  platform_registered: false,
  last_transport_error: "platform connection failed",
};

describe("current diagnostics", () => {
  it("does not expose a platform startup failure in Local", () => {
    const { result, rerender } = renderHook(
      ({ status }) => useDiagnostics(buildRuntimeDiagnostics(proxy, status, config, true)),
      { initialProps: { status: offline } },
    );
    expect(result.current.activeDiagnostics).toEqual([]);
    // A successful native start is authoritative even if the last error text
    // survives in an adjacent-version snapshot.
    rerender({ status: { ...offline, running: true } });
    expect(result.current.activeDiagnostics).toEqual([]);
    rerender({ status: { ...offline, last_error: "" } });
    expect(result.current.activeDiagnostics).toEqual([]);
  });

  it("keeps real application failures while a supplier recovers", () => {
    const { result, rerender } = renderHook(
      ({ status }) => useDiagnostics(buildRuntimeDiagnostics(proxy, status, config, true)),
      { initialProps: { status: offline } },
    );
    act(() => result.current.pushDiagnostic({
      id: "config-load", scope: "about", severity: "error", title: "Config unavailable", message: "read failed", action: "open_logs",
    }));
    rerender({ status: { ...offline, running: true } });
    expect(result.current.activeDiagnostics.map((issue) => issue.id)).toEqual(["config-load"]);
    act(() => result.current.resolveDiagnostic("config-load"));
    expect(result.current.activeDiagnostics).toEqual([]);
  });

  it("does not show startup failure during recovery or after stopping/deleting supply", () => {
    expect(buildRuntimeDiagnostics(proxy, { ...offline, starting: true }, config, true)).toEqual([]);
    for (const channels of [[], [{ ...channel, enabled: false }], [{ ...channel, share_enabled: false }]]) {
      expect(buildRuntimeDiagnostics(proxy, offline, { ...config, channels }, true)).toEqual([]);
    }
    expect(buildRuntimeDiagnostics(proxy, offline, {
      ...config, channels: [{ ...channel, source_driver: "lan_share" }],
    }, true)).toEqual([]);
  });

  it("keeps routing notices on their existing channel surface", () => {
    const status: SupplierStatus = { ...offline, running: true, route_statuses: [
      { channel_id: channel.id, reason: "model_not_supported", unsupported_models: ["test-model"] },
      { channel_id: "channel-b", reason: "pricing_fallback" },
    ] };
    expect(buildRuntimeDiagnostics(proxy, status, config, true)).toEqual([]);
  });

  it("shows local entry errors only while the requested listener is stopped", () => {
    const supplier = { ...offline, last_error: "" };
    const stopped = { ...proxy, running: false };
    expect(buildRuntimeDiagnostics(stopped, supplier, config, true, "address in use")).toMatchObject([
      { id: "local-entry", scope: "access", message: "address in use" },
    ]);
    expect(buildRuntimeDiagnostics(proxy, supplier, config, true, "address in use")).toEqual([]);
    expect(buildRuntimeDiagnostics(stopped, supplier, config, false, "address in use")).toEqual([]);
    expect(buildRuntimeDiagnostics(stopped, supplier, { ...config, proxy_auto_start: false }, true)).toEqual([]);
    expect(buildRuntimeDiagnostics(stopped, supplier, { ...config, proxy_auto_start: false }, true, "manual start failed"))
      .toMatchObject([{ message: "manual start failed" }]);
  });
});

describe("channel runtime details", () => {
  it("keeps another channel's failure visible when one channel is online", () => {
    const status: SupplierStatus = { ...offline, running: true, channels: [
      { ...health, channel_id: "channel-b", status: "available", message: "validation passed" }, health,
    ], channel_transports: [{ ...transport, channel_id: "channel-b", active_transport: "quic", platform_registered: true }] };
    expect(supplierChannelDiagnostic(channel, status)).toMatchObject({ message: "upstream connection failed" });
    expect(supplierChannelDiagnostic({ ...channel, id: "channel-b" }, status)).toBeUndefined();
  });

  it("hides old validation errors after recovery, stopping, or during explicit validation", () => {
    const status = { ...offline, channels: [health] };
    expect(supplierChannelDiagnostic({ ...channel, enabled: false }, status)).toBeUndefined();
    expect(supplierChannelDiagnostic(channel, status, [channel.id])).toBeUndefined();
    expect(supplierChannelDiagnostic(channel, { ...status, channels: [{ ...health, status: "available" }] })).toBeUndefined();
    expect(supplierChannelDiagnostic({ ...channel, id: "different-channel" }, status)).toBeUndefined();
  });

  it("keeps a locally available channel free of platform transport warnings", () => {
    const status = { ...offline, running: true, channels: [{ ...health, status: "available" }], channel_transports: [transport] };
    expect(supplierChannelDiagnostic(channel, status)).toBeUndefined();
    expect(supplierChannelDiagnostic({ ...channel, share_enabled: false }, status)).toBeUndefined();
    expect(supplierChannelDiagnostic({ ...channel, source_driver: "lan_share" }, status)).toBeUndefined();
  });

  it("ignores platform transport errors regardless of registration state", () => {
    const status = { ...offline, running: true, channel_transports: [{
      ...transport, active_transport: "websocket", connected_transport: "websocket", platform_registered: true,
    }] };
    expect(supplierChannelDiagnostic(channel, status)).toBeUndefined();
    expect(supplierChannelDiagnostic(channel, { ...status, channel_transports: [{ ...status.channel_transports[0], platform_registered: false }] }))
      .toBeUndefined();
  });

  it("does not assign a global error to a channel without its own failure evidence", () => {
    expect(supplierChannelDiagnostic(channel, offline)).toBeUndefined();
    expect(supplierChannelDiagnostic(channel, { ...offline, channels: [{ ...health, message: "" }] })).toBeUndefined();
  });

  it.each(["quota_low", "cooldown", "auth_refreshing", "quota_exhausted", "auth_error", "risk_blocked", "disabled"])(
    "does not present successful probe output as the reason for %s", (safetyState) => {
      const status = { ...offline, running: true, channels: [{
        ...health, status: safetyState, safety_state: safetyState, message: "Hello! How can I help you today?",
      }] };
      expect(supplierChannelDiagnostic(channel, status)).toBeUndefined();
      // A platform failure must not affect Local channel status.
      expect(supplierChannelDiagnostic(channel, { ...status, channel_transports: [transport] })).toBeUndefined();
    },
  );

  it("still shows the native validation error status", () => {
    expect(supplierChannelDiagnostic(channel, { ...offline, channels: [{ ...health, status: "error" }] }))
      .toMatchObject({ message: "upstream connection failed" });
  });

  it("renders native details inline and removes the notice once healthy", () => {
    function View({ status }: { status: SupplierStatus }) {
      const issue = supplierChannelDiagnostic(channel, status);
      return issue ? <DiagnosticNotice issue={issue} /> : null;
    }
    const { rerender } = render(<View status={{ ...offline, channels: [health] }} />);
    expect(screen.getByRole("status")).toHaveTextContent("upstream connection failed");
    expect(screen.queryByText("需要处理")).toBeNull();
    rerender(<View status={{ ...offline, channels: [{ ...health, status: "available" }] }} />);
    expect(screen.queryByRole("status")).toBeNull();
  });
});
