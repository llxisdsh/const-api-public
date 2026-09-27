import { describe, expect, test } from "vitest";
import {
  DEFAULT_LOCAL_API_CONNECTION_ID,
  DEFAULT_LOCAL_API_SURFACE_ID,
  LOCAL_API_CONNECTIONS,
  LOCAL_API_SURFACES,
  localApiConnectionById,
  localApiSurfaceById,
} from "./localApiSurfaces";

describe("local API surfaces", () => {
  test("defaults to the OpenAI surface and exposes all public roots", () => {
    expect(DEFAULT_LOCAL_API_SURFACE_ID).toBe("openai");
    expect(LOCAL_API_SURFACES).toEqual([
      { id: "openai", label: "OpenAI", path: "/v1" },
      { id: "anthropic", label: "Anthropic", path: "/anthropic" },
      { id: "gemini", label: "Gemini", path: "/gemini" },
    ]);
  });

  test("falls back to OpenAI for an unknown surface id", () => {
    expect(localApiSurfaceById("missing", "0.0.0.0:19432")).toEqual({
      ...LOCAL_API_SURFACES[0],
      url: "http://127.0.0.1:19432/v1",
    });
  });

  test("derives every public root from the current listen port", () => {
    expect(localApiSurfaceById("anthropic", "127.0.0.1:52109").url)
      .toBe("http://127.0.0.1:52109/anthropic");
    expect(localApiSurfaceById("gemini", "[::1]:43127").url)
      .toBe("http://127.0.0.1:43127/gemini");
  });

  test("offers a full OpenAI Chat URL without treating it as another public surface", () => {
    expect(DEFAULT_LOCAL_API_CONNECTION_ID).toBe("openai");
    expect(LOCAL_API_CONNECTIONS).toEqual([
      { id: "openai", label: "OpenAI", path: "/v1", kind: "base-url" },
      {
        id: "openai-chat",
        label: "OpenAI Chat",
        path: "/v1/chat/completions",
        kind: "request-url",
      },
      { id: "anthropic", label: "Anthropic", path: "/anthropic", kind: "base-url" },
      { id: "gemini", label: "Gemini", path: "/gemini", kind: "base-url" },
    ]);
    expect(LOCAL_API_SURFACES).toHaveLength(3);
    expect(localApiConnectionById("openai-chat", "127.0.0.1:52109").url)
      .toBe("http://127.0.0.1:52109/v1/chat/completions");
  });
});
