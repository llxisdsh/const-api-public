import type { ChannelCapabilityProfile } from "./appTypes";

export type ChannelCapabilityFeatureSupport = "supported" | "experimental" | "limited";

export type ChannelCapabilityFeature = {
  readonly id:
    | "streaming"
    | "toolCalls"
    | "toolControl"
    | "structuredOutput"
    | "reasoning"
    | "vision"
    | "imageOutput"
    | "audioInput"
    | "audioOutput"
    | "videoInput"
    | "videoOutput"
    | "fileInput"
    | "fileOutput"
    | "promptCache"
    | "hostedTools"
    | "customTools";
  readonly support: ChannelCapabilityFeatureSupport;
};

type FeatureCoverage = "none" | "partial" | "complete";

type FeatureDefinition = {
  readonly id: ChannelCapabilityFeature["id"];
  readonly coverage: (profile: ChannelCapabilityProfile) => FeatureCoverage;
};

function completeWhen(value: boolean): FeatureCoverage {
  return value ? "complete" : "none";
}

const FEATURE_DEFINITIONS: readonly FeatureDefinition[] = [
  {
    id: "streaming",
    coverage: (profile) =>
      completeWhen(profile.stream_sse === true && profile.stream_sse_unsupported !== true),
  },
  { id: "toolCalls", coverage: (profile) => completeWhen(profile.tool_calls === true) },
  {
    id: "toolControl",
    coverage: (profile) => {
      const supportedControls = Number(profile.tool_choice === true)
        + Number(profile.parallel_tool_calls === true);
      if (supportedControls === 2) return "complete";
      return supportedControls === 1 ? "partial" : "none";
    },
  },
  { id: "structuredOutput", coverage: (profile) => completeWhen(profile.json_schema === true) },
  {
    id: "reasoning",
    coverage: (profile) =>
      completeWhen(profile.reasoning === true || profile.thinking === true),
  },
  {
    id: "vision",
    coverage: (profile) =>
      completeWhen(profile.vision === true || profile.image_input === true),
  },
  { id: "imageOutput", coverage: (profile) => completeWhen(profile.image_output === true) },
  { id: "audioInput", coverage: (profile) => completeWhen(profile.audio_input === true) },
  { id: "audioOutput", coverage: (profile) => completeWhen(profile.audio_output === true) },
  { id: "videoInput", coverage: (profile) => completeWhen(profile.video_input === true) },
  { id: "videoOutput", coverage: (profile) => completeWhen(profile.video_output === true) },
  { id: "fileInput", coverage: (profile) => completeWhen(profile.file_input === true) },
  { id: "fileOutput", coverage: (profile) => completeWhen(profile.file_output === true) },
  { id: "promptCache", coverage: (profile) => completeWhen(profile.cache_control === true) },
  {
    id: "hostedTools",
    coverage: (profile) => completeWhen((profile.hosted_tools?.length ?? 0) > 0),
  },
  { id: "customTools", coverage: (profile) => completeWhen(profile.custom_tool === true) },
];

function isUsableProfile(profile: ChannelCapabilityProfile): boolean {
  const verificationState = profile.verification_state?.trim().toLowerCase();
  const releaseStatus = profile.release_status?.trim().toLowerCase();
  return profile.protocol.trim().length > 0
    && verificationState !== "failed"
    && verificationState !== "rejected"
    && releaseStatus !== "prepared"
    && releaseStatus !== "suspended";
}

function capabilityLayer(profile: ChannelCapabilityProfile): string {
  const layer = profile.capability_layer?.trim().toLowerCase();
  return layer || "model_wire";
}

function reduceCapabilityFeatures(
  profiles: readonly ChannelCapabilityProfile[],
): readonly ChannelCapabilityFeature[] {
  const usableProfiles = profiles.filter(isUsableProfile);
  const features: ChannelCapabilityFeature[] = [];

  for (const definition of FEATURE_DEFINITIONS) {
    let stableComplete = false;
    let experimentalComplete = false;
    let partial = false;
    for (const profile of usableProfiles) {
      const coverage = definition.coverage(profile);
      if (coverage === "complete") {
        if (profile.release_status?.trim().toLowerCase() === "experimental") {
          experimentalComplete = true;
        } else {
          stableComplete = true;
          break;
        }
      }
      if (coverage === "partial") partial = true;
    }
    if (!stableComplete && !experimentalComplete && !partial) continue;
    features.push({
      id: definition.id,
      support: stableComplete
        ? "supported"
        : experimentalComplete
          ? "experimental"
          : "limited",
    });
  }

  return features;
}

/**
 * Reduces protocol-specific declarations to the user-visible functional scope.
 *
 * Evidence state answers how a declaration was obtained; it does not reduce a
 * declared capability to partial support. "Limited" is reserved for a real
 * functional gap in a composite feature.
 */
export function channelCapabilityFeatures(
  profiles: readonly ChannelCapabilityProfile[],
): readonly ChannelCapabilityFeature[] {
  return reduceCapabilityFeatures(profiles.filter((profile) =>
    ["model_wire", "driver"].includes(capabilityLayer(profile))));
}

export function channelClientHostFeatures(
  profiles: readonly ChannelCapabilityProfile[],
): readonly ChannelCapabilityFeature[] {
  return reduceCapabilityFeatures(
    profiles.filter((profile) => capabilityLayer(profile) === "client_host"),
  );
}
