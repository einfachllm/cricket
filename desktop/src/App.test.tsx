import { render, screen } from "@testing-library/react"
import { expect, test } from "vitest"
import App from "./App"

test("renders heading", () => {
  render(<App />)
  const headingElement = screen.getByText(/Harnesswurm/i)
  expect(headingElement).toBeInTheDocument()
})

test("renders backend readiness", () => {
  render(<App />)
  const statusElement = screen.getByText(/Backend ready/i)
  expect(statusElement).toBeInTheDocument()
})
