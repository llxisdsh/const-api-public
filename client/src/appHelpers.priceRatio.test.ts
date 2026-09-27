import { describe, expect, test } from "vitest";

import {
  normalizedPriceRatio,
  priceRatioFromInput,
  priceRatioToDisplay,
} from "./appHelpers";

describe("price ratio normalization", () => {
  test("preserves zero as an explicit free price", () => {
    expect(normalizedPriceRatio(0)).toBe(0);
    expect(priceRatioFromInput("0")).toBe(0);
    expect(priceRatioToDisplay(0)).toBe("0");
  });

  test("clamps negative values and defaults invalid values", () => {
    expect(normalizedPriceRatio(-0.5)).toBe(0);
    expect(priceRatioFromInput("-2")).toBe(0);
    expect(normalizedPriceRatio(100)).toBe(3);
    expect(priceRatioFromInput("100")).toBe(3);
    expect(normalizedPriceRatio(undefined)).toBe(1);
    expect(priceRatioFromInput("not-a-number")).toBe(1);
  });
});
