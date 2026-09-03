import { render, screen } from "@testing-library/react"
import { expect, test } from "vitest"
import App from "./App"

test("renders heading", () => {
  render(<App />)
  const headingElement = screen.getByText(/Harnesswurm/i)
  expect(headingElement).toBeInTheDocument()
})

test("renders backend readiness", async () => {
  render(<App />)
  const statusElement = await screen.findByText(/Backend (ready|unreachable)/i)
  expect(statusElement).toBeInTheDocument()
})
