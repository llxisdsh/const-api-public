import { localLoopbackApiRoot } from "./appHelpers";

export const DEFAULT_LOCAL_API_SURFACE_ID = "openai" as const;

export const LOCAL_API_SURFACES = [
  { id: "openai", label: "OpenAI", path: "/v1" },
  { id: "anthropic", label: "Anthropic", path: "/anthropic" },
  { id: "gemini", label: "Gemini", path: "/gemini" },
] as const;

export type LocalApiSurfaceId = (typeof LOCAL_API_SURFACES)[number]["id"];

export function localApiSurfaceById(value: string | null | undefined, listen: string) {
  const surface = LOCAL_API_SURFACES.find((candidate) => candidate.id === value) ?? LOCAL_API_SURFACES[0];
  const root = localLoopbackApiRoot(listen);
  return {
    ...surface,
    url: root ? `${root}${surface.path}` : "",
  };
}

export const DEFAULT_LOCAL_API_CONNECTION_ID = "openai" as const;

export const LOCAL_API_CONNECTIONS = [
  { ...LOCAL_API_SURFACES[0], kind: "base-url" },
  {
    id: "openai-chat",
    label: "OpenAI Chat",
    path: `${LOCAL_API_SURFACES[0].path}/chat/completions`,
    kind: "request-url",
  },
  { ...LOCAL_API_SURFACES[1], kind: "base-url" },
  { ...LOCAL_API_SURFACES[2], kind: "base-url" },
] as const;

export type LocalApiConnectionId = (typeof LOCAL_API_CONNECTIONS)[number]["id"];

export function localApiConnectionById(value: string | null | undefined, listen: string) {
  const connection = LOCAL_API_CONNECTIONS.find((candidate) => candidate.id === value)
    ?? LOCAL_API_CONNECTIONS[0];
  const root = localLoopbackApiRoot(listen);
  return {
    ...connection,
    url: root ? `${root}${connection.path}` : "",
  };
}
