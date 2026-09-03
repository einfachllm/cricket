import { expect, test } from "vitest";
import { tokensByModel } from "./AnalyticsDashboard";

test("tokensByModel sums prompt+completion per model, nulls to unknown", () => {
  const rows = [
    { model_name: "gpt-4o", prompt_tokens: 100, completion_tokens: 50 },
    { model_name: "gpt-4o", prompt_tokens: 0, completion_tokens: 10 },
    { model_name: null, prompt_tokens: 5, completion_tokens: 5 },
  ] as any;
  expect(tokensByModel(rows)).toEqual([
    { model: "gpt-4o", tokens: 160 },
    { model: "unknown", tokens: 10 },
  ]);
});
