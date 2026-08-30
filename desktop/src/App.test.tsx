import { render, screen } from "@testing-library/react"
import { expect, test } from "vitest"
import App from "./App"

test("renders heading", () => {
  render(<App />)
  const headingElement = screen.getByText(/Agent-Turn/i)
  expect(headingElement).toBeInTheDocument()
})

test("renders status", () => {
  render(<App />)
  const statusElement = screen.getByText(/Status:/i)
  expect(statusElement).toBeInTheDocument()
})
