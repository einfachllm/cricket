import { expect, test } from "vitest";
import { render, screen } from "@testing-library/react";
import { filterTasks } from "./TrafficView";
import { CollapsibleJson } from "./CollapsibleJson";

const rows = [
  { task_id: 1, agent_name: "opencode", task_description: "fix login", agent_question_text: null },
  { task_id: 2, agent_name: "claude", task_description: "refactor db", agent_question_text: "which table?" },
] as any;

test("filter matches agent, description, and questionsOnly", () => {
  expect(filterTasks(rows, "login", "all", false)).toHaveLength(1);
  expect(filterTasks(rows, "", "claude", false)).toHaveLength(1);
  expect(filterTasks(rows, "", "all", true)).toHaveLength(1);
});

test("json tree renders top-level keys collapsed-safe", () => {
  render(<CollapsibleJson raw='{"a":1,"b":{"c":[1,2]}}' />);
  expect(screen.getByText("a")).toBeInTheDocument();
  expect(screen.getByText("b")).toBeInTheDocument();
});
