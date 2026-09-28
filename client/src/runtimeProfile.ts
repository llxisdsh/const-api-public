export function developmentProfileEnabled(dev: boolean, profile: string | undefined) {
  return dev && profile === "development";
}

export const DEVELOPMENT_PROFILE = developmentProfileEnabled(
  import.meta.env.DEV,
  import.meta.env.VITE_CONST_API_PROFILE,
);

// The public exporter changes this single edition capability, not the runtime profile.
export const RELEASE_UPDATES_SUPPORTED = false;
export const RELEASE_UPDATES_ENABLED = RELEASE_UPDATES_SUPPORTED && !DEVELOPMENT_PROFILE;

export const DEFAULT_LOCAL_PROXY_LISTEN = DEVELOPMENT_PROFILE
  ? "127.0.0.1:38789"
  : "127.0.0.1:38789";

export const PRODUCT_DISPLAY_NAME = "CONST API Local";

export const APPLICATION_DISPLAY_NAME = DEVELOPMENT_PROFILE
  ? `${PRODUCT_DISPLAY_NAME} Dev`
  : PRODUCT_DISPLAY_NAME;

export function defaultDevelopmentEndpoint(development: boolean) {
  return development ? "local-dev" : "auto";
}

export const DEFAULT_DEVELOPMENT_ENDPOINT = defaultDevelopmentEndpoint(DEVELOPMENT_PROFILE);
