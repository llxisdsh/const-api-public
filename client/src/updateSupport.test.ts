import { describe, expect, it } from "vitest";
import {
  formatUpdateError,
  isRetryableUpdateTransportError,
  updateInstallWaitingText,
} from "./updateSupport";

describe("update support", () => {
  it("classifies updater transport failures", () => {
    expect(isRetryableUpdateTransportError("error sending request for url (https://example.test/latest.json)")).toBe(true);
    expect(isRetryableUpdateTransportError("Download request failed with status: 503 Service Unavailable")).toBe(true);
    expect(isRetryableUpdateTransportError("all configured update sources failed: unavailable")).toBe(true);
    expect(isRetryableUpdateTransportError("signature verification failed")).toBe(false);
  });

  it("turns transport errors into a concise actionable message", () => {
    expect(formatUpdateError("error sending request for url (https://example.test/latest.json)"))
      .toBe("暂时无法连接可用的更新源；已自动尝试备用来源，请检查网络后再试。");
  });

  it("explains why a downloaded update is still waiting", () => {
    expect(updateInstallWaitingText({
      idle: false,
      gate_acquired: false,
      draining: false,
      active_logical_requests: 2,
      active_proxy_requests: 0,
      active_supplier_requests: 2,
      pending_supplier_outbound_messages: 0,
      unacknowledged_supplier_replay_messages: 0,
      reason: "active_supplier_requests",
    })).toBe("正在等待 2 个请求完成");

    expect(updateInstallWaitingText({
      idle: false,
      gate_acquired: false,
      draining: false,
      active_logical_requests: 0,
      active_proxy_requests: 0,
      active_supplier_requests: 0,
      pending_supplier_outbound_messages: 0,
      unacknowledged_supplier_replay_messages: 3,
      reason: "unacknowledged_supplier_replay_messages",
    })).toBe("正在等待 3 条供应响应被服务器确认");
  });
});
