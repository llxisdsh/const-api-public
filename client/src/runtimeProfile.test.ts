import { describe, expect, it } from "vitest";
import {
  defaultDevelopmentEndpoint,
  developmentProfileEnabled,
} from "./runtimeProfile";

describe("runtime profile", () => {
  it("enables isolation only for the explicit Vite development profile", () => {
    expect(developmentProfileEnabled(true, "development")).toBe(true);
    expect(developmentProfileEnabled(true, undefined)).toBe(false);
    expect(developmentProfileEnabled(true, "production")).toBe(false);
    expect(developmentProfileEnabled(false, "development")).toBe(false);
  });

  it("defaults only the development profile to the local server", () => {
    expect(defaultDevelopmentEndpoint(true)).toBe("local-dev");
    expect(defaultDevelopmentEndpoint(false)).toBe("auto");
  });
});
