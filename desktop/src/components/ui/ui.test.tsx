import { render, screen } from "@testing-library/react";
import { expect, test } from "vitest";
import { Card } from "./Card";
import { Empty } from "./Empty";

test("card renders surface children", () => {
  render(<Card><span>hello card</span></Card>);
  expect(screen.getByText("hello card")).toBeInTheDocument();
});

test("empty renders title and hint", () => {
  render(<Empty title="No traffic" hint="Point an agent at the proxy" />);
  expect(screen.getByText("No traffic")).toBeInTheDocument();
  expect(screen.getByText("Point an agent at the proxy")).toBeInTheDocument();
});
