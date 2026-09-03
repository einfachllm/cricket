import { expect, test } from "vitest";
import { DARK } from "./tokens";
import { formatTimestampUtc } from "../lib/api";

test("dark tokens expose shell surfaces", () => {
  expect(DARK.appBg).toBe("#0b0e14");
  expect(DARK.cardBg).toBe("#151a23");
  expect(DARK.sidebarBg).toBe("#11131a");
});

test("timestamp helper formats UTC without Z hack at call site", () => {
  expect(formatTimestampUtc("2026-09-03T10:20:30")).toBe(
    new Date("2026-09-03T10:20:30Z").toLocaleTimeString()
  );
});
