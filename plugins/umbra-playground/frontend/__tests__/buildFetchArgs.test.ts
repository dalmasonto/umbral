import { describe, it, expect } from "vitest";
import { buildFetchArgs } from "../state/buildFetchArgs";
import type { RequestDraft } from "../state/store";

function draft(overrides: Partial<RequestDraft> = {}): RequestDraft {
  return {
    method: "GET",
    url: "/api/articles/",
    params: {},
    headers: {},
    body: "",
    bearerToken: "",
    ...overrides,
  };
}

describe("buildFetchArgs", () => {
  it("returns the URL unchanged for a path with no template params", () => {
    const result = buildFetchArgs(draft());
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.args.url).toBe("/api/articles/");
    }
  });

  it("resolves path template params from the params map", () => {
    const result = buildFetchArgs(
      draft({ url: "/api/articles/{id}/", params: { id: "42" } }),
    );
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.args.url).toBe("/api/articles/42/");
    }
  });

  it("errors when a path template param is missing", () => {
    const result = buildFetchArgs(draft({ url: "/api/articles/{id}/" }));
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.kind).toBe("missing_path_param");
      if (result.error.kind === "missing_path_param") {
        expect(result.error.name).toBe("id");
      }
    }
  });

  it("appends query params to the URL", () => {
    const result = buildFetchArgs(
      draft({ url: "/api/articles/", params: { page: "2", limit: "10" } }),
    );
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.args.url).toContain("page=2");
      expect(result.args.url).toContain("limit=10");
    }
  });

  it("encodes special characters in query values", () => {
    const result = buildFetchArgs(
      draft({ url: "/api/articles/", params: { q: "hello world" } }),
    );
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.args.url).toContain("q=hello%20world");
    }
  });

  it("adds bearer token as Authorization header", () => {
    const result = buildFetchArgs(draft({ bearerToken: "abc123" }));
    expect(result.ok).toBe(true);
    if (result.ok) {
      const headers = result.args.init.headers as Record<string, string>;
      expect(headers["Authorization"]).toBe("Bearer abc123");
    }
  });

  it("serializes a JSON body for POST", () => {
    const result = buildFetchArgs(
      draft({ method: "POST", url: "/api/articles/", body: '{"title":"x"}' }),
    );
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.args.init.method).toBe("POST");
      expect(result.args.init.body).toBe('{"title":"x"}');
    }
  });

  it("errors on invalid JSON body", () => {
    const result = buildFetchArgs(
      draft({ method: "POST", url: "/api/articles/", body: "{not json" }),
    );
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.kind).toBe("invalid_json_body");
    }
  });

  it("does not send a body for GET", () => {
    const result = buildFetchArgs(
      draft({ method: "GET", url: "/api/articles/", body: "ignored" }),
    );
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.args.init.body).toBeUndefined();
    }
  });

  it("merges user headers", () => {
    const result = buildFetchArgs(
      draft({ headers: { "X-Custom": "yes" } }),
    );
    expect(result.ok).toBe(true);
    if (result.ok) {
      const headers = result.args.init.headers as Record<string, string>;
      expect(headers["X-Custom"]).toBe("yes");
    }
  });
});
