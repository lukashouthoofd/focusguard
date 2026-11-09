import { expect, it } from "vitest";
import { appRule } from "./match-types";

it("creates a bundle identifier rule", () => {
  expect(appRule("BundleId", "com.example.Focus")).toEqual({
    BundleId: "com.example.Focus",
  });
});
