import { describe, expect, test } from "vitest";
import {
  lanShareConnectionHost,
  lanShareOpenAiBaseUrl,
  parseLanShareConnectionInfo,
} from "./lanShareConnectionInfo";

describe("LAN sharing connection information", () => {
  test("builds the OpenAI Base URL copied to a member", () => {
    expect(lanShareOpenAiBaseUrl("http://192.168.1.20:38788")).toBe("http://192.168.1.20:38788/v1");
    expect(lanShareOpenAiBaseUrl("http://192.168.1.20:38788/v1/")).toBe("http://192.168.1.20:38788/v1");
  });

  test("extracts the host used in the generated channel name", () => {
    expect(lanShareConnectionHost("http://192.168.1.20:38788/v1")).toBe("192.168.1.20");
    expect(lanShareConnectionHost("not a URL")).toBe("");
  });

  test("parses the complete Chinese connection message", () => {
    expect(parseLanShareConnectionInfo([
      "CONST API 局域网共享渠道",
      "渠道名称: 局域网共享 · 192.168.1.20",
      "Base URL: http://192.168.1.20:38788/v1",
      "API Key: cst-lan-alice",
      "每周 Token 上限: 10,000",
      "",
      "使用方法:",
      "1. 打开模型页。",
    ].join("\n"))).toEqual({
      channelName: "局域网共享 · 192.168.1.20",
      baseUrl: "http://192.168.1.20:38788/v1",
      apiKey: "cst-lan-alice",
    });
  });

  test("parses English field names and full-width separators", () => {
    expect(parseLanShareConnectionInfo([
      "CONST API LAN Sharing Channel",
      "Channel Name：LAN sharing · Bob",
      "Base URL：https://host.example/v1",
      "API Key：cst-lan-bob",
    ].join("\r\n"))).toEqual({
      channelName: "LAN sharing · Bob",
      baseUrl: "https://host.example/v1",
      apiKey: "cst-lan-bob",
    });
  });

  test("rejects unrelated or incomplete text", () => {
    expect(parseLanShareConnectionInfo("Base URL: https://api.example/v1\nAPI Key: secret")).toBeNull();
    expect(parseLanShareConnectionInfo("CONST API 局域网共享渠道\n渠道名称: Alice\nBase URL: bad\nAPI Key: key")).toBeNull();
  });
});
