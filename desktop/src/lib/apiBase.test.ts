import { expect, test, vi, afterEach } from "vitest";
import { setApiBase, resetApiBaseForTests, resolvedApiBase } from "./api";

afterEach(() => { resetApiBaseForTests(); vi.unstubAllEnvs(); });

test("explicit setter wins over everything", () => {
  vi.stubEnv("VITE_HARNESSWURM_API_BASE", "http://env:9999");
  setApiBase("http://tauri:8082");
  expect(resolvedApiBase()).toBe("http://tauri:8082");
});

test("vite env override beats the default", () => {
  vi.stubEnv("VITE_HARNESSWURM_API_BASE", "http://env:9999");
  expect(resolvedApiBase()).toBe("http://env:9999");
});

test("default is localhost 8081", () => {
  expect(resolvedApiBase()).toBe("http://localhost:8081");
});
