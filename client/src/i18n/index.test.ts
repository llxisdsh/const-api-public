import { beforeAll, describe, expect, it } from "vitest";

import {
  changeAppLanguage,
  initializeI18n,
  localizedError,
  stableErrorCode,
} from "./index";

beforeAll(async () => {
  await initializeI18n();
});

describe("localizedError", () => {
  it("uses the active language for a structured error code", async () => {
    await changeAppLanguage("zh-CN");
    expect(localizedError({ code: "insufficient_balance", params: {} })).toBe(
      "使用余额不足，本次请求未完成。请充值到账后重试。",
    );
    expect(localizedError('{"code":"finance_unavailable","params":{}}')).toBe(
      "资金服务暂时不可用，请稍后再试。",
    );
  });

  it("extracts a stable code from a wrapped native error", () => {
    expect(stableErrorCode(new Error('{"code":"idempotency_conflict","params":{}}'))).toBe(
      "idempotency_conflict",
    );
  });

  it("falls back to English when the active resource has no code", async () => {
    await changeAppLanguage("en-US");
    expect(localizedError('{"code":"service_unavailable","params":{}}')).toBe(
      "The service is temporarily unavailable. Try again later.",
    );
  });

  it("keeps structured parameters when a JSON payload is wrapped in Error", async () => {
    await changeAppLanguage("en-US");
    expect(localizedError(new Error('{"code":"retry_after","params":{"seconds":5}}'))).toBe(
      "Try again in 5 seconds.",
    );
  });

  it("keeps structured parameters behind a transport prefix", async () => {
    await changeAppLanguage("en-US");
    expect(localizedError(new Error(
      'request failed: {"error":{"code":"retry_after","params":{"seconds":11}}}',
    ))).toBe("Try again in 11 seconds.");
  });

  it("shows the precise wait returned by an account rate limit", async () => {
    await changeAppLanguage("zh-CN");
    expect(localizedError(new Error(
      '{"code":"try_later","params":{"retry_after_seconds":43}}',
    ))).toBe("请在 43 秒后重试。");
  });

  it("turns an account gateway failure into an actionable message", async () => {
    await changeAppLanguage("en-US");
    expect(localizedError("account_http_502")).toBe(
      "Could not reach the platform account service. Check the network or confirm that the server is running, then try again.",
    );
  });

  it("classifies similar account HTTP failures without exposing machine codes", async () => {
    await changeAppLanguage("en-US");
    expect(localizedError("account_http_429")).toBe(
      "Too many account operations. Try again later.",
    );
    expect(localizedError(new Error("request failed: account_http_504"))).toBe(
      "The platform account service timed out. Try again later.",
    );
    expect(localizedError("account_http_418")).toBe(
      "The platform account request failed (HTTP 418). Try again later.",
    );
    expect(localizedError("auth_required")).toBe(
      "Sign in to the platform account first.",
    );
  });

  it("localizes a stable native code embedded in a mixed-language message", async () => {
    await changeAppLanguage("en-US");
    expect(localizedError(
      "强制结束 OpenCode 失败: TOOL_CONFIG_CONFIRMATION_PROCESS_REPLACED: an authorized process changed",
    )).toBe("The authorized process changed before it could be stopped safely.");
  });

  it("preserves the original server message for an unknown code", async () => {
    await changeAppLanguage("zh-CN");
    const raw = '{"code":"future_server_error","params":{"wait":5}}';
    expect(localizedError(raw)).toBe(raw);
  });

  it("localizes manual-close reasons without leaking backend prose", async () => {
    for (const language of ["zh-CN", "en-US"] as const) {
      await changeAppLanguage(language);
      expect(localizedError("TOOL_CONFIG_PROCESS_STILL_RUNNING")).toBe(language === "zh-CN"
        ? "仍检测到程序或其后台进程，可能正在退出或被其他程序重新启动。本次尚未写入配置。"
        : "The program or a background process is still running. It may be shutting down or have been restarted by another program. No configuration has been written.");
      expect(localizedError("TOOL_CONFIG_PROCESS_PROBE_FAILED: verify Codex remained closed")).toBe(language === "zh-CN"
        ? "无法检查正在运行的程序。"
        : "The running program could not be checked.");
      expect(localizedError("TOOL_CONFIG_PROCESS_CLOSE_FAILED: native termination failed")).toBe(language === "zh-CN"
        ? "无法安全关闭程序。"
        : "The program could not be closed safely.");
    }
  });
});
