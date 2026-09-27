import { describe, expect, it } from "vitest";
import {
  applySupplierUpstreamObservation,
  endpointNetworkState,
  localListenAddressForLanAccess,
  localLoopbackApiRoot,
  supplierChannelRuntimeStarted,
  supplierNetworkState,
  supplierProtocolLatencyLabel,
  supplierProtocolLatencyParts,
} from "./appHelpers";
import type { SupplierStatus } from "./appTypes";

const supplierOffline: SupplierStatus = {
  running: false,
  node_id: "",
  server_ws_url: "",
  server_quic_url: "",
  transport_preference: "",
  connected_transport: "",
  active_transport: "",
};

describe("endpointNetworkState", () => {
  it("reports the platform request transport without depending on supplier state", () => {
    expect(
      endpointNetworkState(
        {
          running: true,
          listen: "127.0.0.1:38787",
          active_endpoint: "local-dev",
          platform_connection_state: "established",
          platform_transport: "http3",
        },
      ),
    ).toBe("请求 HTTP/3");
  });

  it("makes an HTTP/3 fallback visible", () => {
    expect(
      endpointNetworkState(
        {
          running: true,
          listen: "127.0.0.1:38787",
          active_endpoint: "local-dev",
          platform_connection_state: "established",
          platform_transport: "http2",
          platform_transport_error: "UDP unavailable",
        },
      ),
    ).toBe("请求 HTTP/2（HTTP/3 已回退）");
  });
});

describe("supplierNetworkState", () => {
  it("shows the established supplier transport independently from platform HTTP", () => {
    expect(supplierNetworkState({
      ...supplierOffline,
      running: true,
      connected_transport: "quic",
      active_transport: "quic",
    })).toBe("供应 QUIC");
  });

  it("shows the transport while platform registration is still pending", () => {
    expect(supplierNetworkState({
      ...supplierOffline,
      running: true,
      connected_transport: "websocket",
      active_transport: "starting",
    })).toBe("供应 WebSocket（确认中）");
  });

  it("makes reconnecting and stopped states visible", () => {
    expect(supplierNetworkState({
      ...supplierOffline,
      running: true,
      connected_transport: "reconnecting",
      active_transport: "reconnecting",
    })).toBe("供应重连中");
    expect(supplierNetworkState(supplierOffline)).toBe("供应未启动");
  });
});

describe("supplierProtocolLatencyLabel", () => {
  it("keeps the negotiated HTTP version and probe latency together", () => {
    expect(supplierProtocolLatencyLabel({ upstream_http_version: "http3", latency_ms: 187 })).toBe("HTTP/3 · 187ms");
    expect(supplierProtocolLatencyLabel({ upstream_http_version: "http2", latency_ms: 0 })).toBe("HTTP/2");
    expect(supplierProtocolLatencyLabel({ upstream_http_version: "", latency_ms: 42 })).toBe("42ms");
    expect(supplierProtocolLatencyLabel()).toBe("-");
  });

  it("provides separate primary and secondary values for the detail summary", () => {
    expect(supplierProtocolLatencyParts({ upstream_http_version: "http2", latency_ms: 27 })).toEqual({
      primary: "HTTP/2",
      secondary: "27ms",
    });
    expect(supplierProtocolLatencyParts({ upstream_http_version: "", latency_ms: 42 })).toEqual({
      primary: "-",
      secondary: "42ms",
    });
  });
});

describe("applySupplierUpstreamObservation", () => {
  const status: SupplierStatus = {
    ...supplierOffline,
    channels: [{
      channel_id: "channel-a",
      name: "Channel A",
      status: "available",
      model: "model-a",
      model_count: 1,
      latency_ms: 40,
      upstream_http_version: "http3",
      credential_status: "ok",
      quota_status: "unknown",
      message: "",
      checked_at_unix: 1,
    }],
  };

  it("updates the detail health from a successful manual upstream test", () => {
    const next = applySupplierUpstreamObservation(status, "channel-a", {
      upstream_http_version: "http2",
      latency_ms: 187,
    });

    expect(next.channels?.[0]).toMatchObject({
      upstream_http_version: "http2",
      latency_ms: 187,
    });
  });

  it("does not erase the last protocol when a later result has no observation", () => {
    const next = applySupplierUpstreamObservation(status, "channel-a", {
      upstream_http_version: "",
      latency_ms: 63,
    });

    expect(next.channels?.[0]).toMatchObject({
      upstream_http_version: "http3",
      latency_ms: 63,
    });
  });
});

describe("localListenAddressForLanAccess", () => {
  it("changes only the host when LAN access is enabled", () => {
    expect(localListenAddressForLanAccess("127.0.0.1:19432", true)).toBe("0.0.0.0:19432");
  });

  it("changes only the host when LAN access is disabled", () => {
    expect(localListenAddressForLanAccess("0.0.0.0:52109", false)).toBe("127.0.0.1:52109");
  });

  it("preserves a manually edited port after an IPv6 host", () => {
    expect(localListenAddressForLanAccess("[::1]:43127", true)).toBe("0.0.0.0:43127");
  });

  it("does not invent a port for an incomplete address", () => {
    expect(localListenAddressForLanAccess("127.0.0.1", true)).toBeNull();
  });
});

describe("localLoopbackApiRoot", () => {
  it("uses the configured port but always exposes a loopback host", () => {
    expect(localLoopbackApiRoot("0.0.0.0:52109")).toBe("http://127.0.0.1:52109");
    expect(localLoopbackApiRoot("192.168.1.23:43127")).toBe("http://127.0.0.1:43127");
  });

  it("returns null when the configured address has no port", () => {
    expect(localLoopbackApiRoot("127.0.0.1")).toBeNull();
  });
});

describe("supplierChannelRuntimeStarted", () => {
  it("does not treat an enabled or globally running supplier as this channel being started", () => {
    expect(supplierChannelRuntimeStarted({ ...supplierOffline, running: true }, "channel-a")).toBe(false);
  });

  it("uses the backend runtime agent projection even while its transport is disconnected", () => {
    expect(
      supplierChannelRuntimeStarted(
        {
          ...supplierOffline,
          running: true,
          channel_transports: [{
            channel_id: "channel-a",
            supplier_unit_id: "unit-a",
            connected_transport: "",
            active_transport: "",
            last_transport_error: "reconnecting",
          }],
        },
        "channel-a",
      ),
    ).toBe(true);
    expect(
      supplierChannelRuntimeStarted(
        {
          ...supplierOffline,
          running: true,
          channel_transports: [{
            channel_id: "channel-a",
            supplier_unit_id: "unit-a",
            connected_transport: "",
            active_transport: "",
            last_transport_error: "",
          }],
        },
        "channel-b",
      ),
    ).toBe(false);
  });
});
