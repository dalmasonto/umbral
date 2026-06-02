import { describe, it, expect, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import { usePlayground } from "../state/store";
import { RequestBuilder } from "../components/RequestBuilder";
import type { OpenAPIV3 } from "openapi-types";

const SPEC: OpenAPIV3.Document = {
  openapi: "3.0.0",
  info: { title: "test", version: "0.0.1" },
  paths: {
    "/api/articles/{id}/": {
      get: {
        operationId: "get-article",
        responses: { "200": { description: "ok" } },
      },
      delete: {
        operationId: "delete-article",
        responses: { "204": { description: "no content" } },
      },
    },
  },
} as OpenAPIV3.Document;

describe("RequestBuilder", () => {
  beforeEach(() => {
    usePlayground.setState({
      spec: SPEC,
      specError: null,
      loadingSpec: false,
      selectedOperationId: null,
      current: {
        method: "GET",
        url: "",
        params: {},
        headers: {},
        body: "",
        bearerToken: "",
      },
      lastResponse: null,
      inFlight: false,
      history: {},
    });
  });

  it("shows an empty state when no endpoint is selected", () => {
    render(<RequestBuilder />);
    expect(screen.getByText(/select an endpoint/i)).toBeInTheDocument();
  });

  it("populates the URL strip with the selected operation's path", async () => {
    usePlayground.setState({ selectedOperationId: "get-article" });
    render(<RequestBuilder />);
    await waitFor(() => {
      expect(screen.getByDisplayValue("/api/articles/{id}/")).toBeInTheDocument();
    });
  });

  it("renders path-template inputs when the path has {placeholders}", async () => {
    usePlayground.setState({ selectedOperationId: "get-article" });
    render(<RequestBuilder />);
    await waitFor(() => {
      expect(screen.getByText("id")).toBeInTheDocument();
    });
  });
});
