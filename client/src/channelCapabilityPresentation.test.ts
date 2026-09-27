import { describe, expect, it } from "vitest";

import type { ChannelCapabilityProfile } from "./appTypes";
import {
  channelCapabilityFeatures,
  channelClientHostFeatures,
} from "./channelCapabilityPresentation";

function featureSupport(
  profiles: readonly ChannelCapabilityProfile[],
  featureID: string,
): string | undefined {
  return channelCapabilityFeatures(profiles).find(({ id }) => id === featureID)?.support;
}

describe("channelCapabilityFeatures", () => {
  it("does not confuse declared evidence with limited functionality", () => {
    const profiles: ChannelCapabilityProfile[] = [{
      protocol: "openai_responses",
      stream_sse: true,
      tool_calls: true,
      tool_choice: true,
      parallel_tool_calls: true,
      json_schema: true,
      reasoning: true,
      vision: true,
      verification_state: "declared",
    }];

    expect(channelCapabilityFeatures(profiles)).toEqual([
      { id: "streaming", support: "supported" },
      { id: "toolCalls", support: "supported" },
      { id: "toolControl", support: "supported" },
      { id: "structuredOutput", support: "supported" },
      { id: "reasoning", support: "supported" },
      { id: "vision", support: "supported" },
    ]);
  });

  it("uses limited only for an actual partial composite feature", () => {
    expect(featureSupport([{
      protocol: "anthropic_messages",
      tool_choice: true,
      verification_state: "verified",
    }], "toolControl")).toBe("limited");
  });

  it("lets complete coverage win across protocols", () => {
    expect(featureSupport([
      {
        protocol: "anthropic_messages",
        tool_choice: true,
        verification_state: "declared",
      },
      {
        protocol: "openai_responses",
        tool_choice: true,
        parallel_tool_calls: true,
        verification_state: "verified",
      },
    ], "toolControl")).toBe("supported");
  });

  it("ignores failed and rejected profiles and explicit streaming rejection", () => {
    const features = channelCapabilityFeatures([
      {
        protocol: "openai_responses",
        audio_input: true,
        verification_state: "failed",
      },
      {
        protocol: "anthropic_messages",
        video_output: true,
        verification_state: "rejected",
      },
      {
        protocol: "gemini_native",
        stream_sse: true,
        stream_sse_unsupported: true,
        verification_state: "verified",
      },
    ]);

    expect(features).toEqual([]);
  });

  it("shows only live experimental capabilities and hides prepared or suspended ones", () => {
    expect(featureSupport([
      {
        protocol: "openai_responses",
        audio_input: true,
        release_status: "prepared",
        verification_state: "declared",
      },
      {
        protocol: "gemini_native",
        video_input: true,
        release_status: "experimental",
        verification_state: "declared",
      },
      {
        protocol: "gemini_native",
        audio_output: true,
        release_status: "suspended",
        verification_state: "verified",
      },
    ], "videoInput")).toBe("experimental");
    expect(featureSupport([
      {
        protocol: "openai_responses",
        audio_input: true,
        release_status: "prepared",
        verification_state: "declared",
      },
    ], "audioInput")).toBeUndefined();
  });

  it("keeps client-host abilities out of the routable API projection", () => {
    const profiles: ChannelCapabilityProfile[] = [{
      protocol: "openai_responses",
      capability_layer: "client_host",
      file_input: true,
      verification_state: "declared",
    }];

    expect(channelCapabilityFeatures(profiles)).toEqual([]);
    expect(channelClientHostFeatures(profiles)).toEqual([
      { id: "fileInput", support: "supported" },
    ]);
  });
});
