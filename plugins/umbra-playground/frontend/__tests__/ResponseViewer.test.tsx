import { describe, it, expect, beforeEach } from "vitest";
import { render, screen } from "@testing-library/react";
import { usePlayground, type ResponseRecord } from "../state/store";
import { ResponseViewer } from "../components/ResponseViewer";

function record(overrides: Partial<ResponseRecord> = {}): ResponseRecord {
  return {
    operationId: "test",
    request: {
      method: "GET",
      url: "/api/x/",
      params: {},
      headers: {},
      body: "",
      bearerToken: "",
    },
    status: 200,
    statusText: "OK",
    durationMs: 42,
    sizeBytes: 100,
    headers: { "content-type": "application/json" },
    bodyText: '{"hello":"world"}',
    timestamp: Date.now(),
    ...overrides,
  };
}

describe("ResponseViewer", () => {
  beforeEach(() => {
    usePlayground.setState({
      lastResponse: null,
      history: {},
      selectedOperationId: "test",
    });
  });

  it("shows empty state when no response has been recorded", () => {
    render(<ResponseViewer />);
    expect(screen.getByText(/send a request/i)).toBeInTheDocument();
  });

  it("renders a 2xx status in emerald", () => {
    usePlayground.setState({ lastResponse: record({ status: 200 }) });
    const { container } = render(<ResponseViewer />);
    expect(screen.getByText("200")).toBeInTheDocument();
    expect(container.querySelector(".text-emerald-300")).toBeInTheDocument();
  });

  it("renders a 4xx status in rose", () => {
    usePlayground.setState({ lastResponse: record({ status: 404 }) });
    const { container } = render(<ResponseViewer />);
    expect(screen.getByText("404")).toBeInTheDocument();
    expect(container.querySelector(".text-rose-300")).toBeInTheDocument();
  });
});
