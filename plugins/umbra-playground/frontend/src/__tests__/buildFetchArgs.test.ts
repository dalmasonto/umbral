import { describe, it, expect } from "vitest";
import { buildFetchArgs } from "../state/buildFetchArgs";
import type { RequestDraft } from "../state/store";

function draft(overrides: Partial<RequestDraft> = {}): RequestDraft {
  return {
    method: "GET",
    url: "/api/articles/",
    params: [],
    headers: [],
    bodyType: "json",
    body: "",
    formFields: [],
    authScheme: "Bearer",
    authToken: "",
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

  it("resolves path template params from the params list", () => {
    const result = buildFetchArgs(
      draft({
        url: "/api/articles/{id}/",
        params: [{ key: "id", value: "42", enabled: true }],
      }),
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
      draft({
        url: "/api/articles/",
        params: [
          { key: "page", value: "2", enabled: true },
          { key: "limit", value: "10", enabled: true },
        ],
      }),
    );
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.args.url).toContain("page=2");
      expect(result.args.url).toContain("limit=10");
    }
  });

  it("encodes special characters in query values", () => {
    const result = buildFetchArgs(
      draft({
        url: "/api/articles/",
        params: [{ key: "q", value: "hello world", enabled: true }],
      }),
    );
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.args.url).toContain("q=hello%20world");
    }
  });

  it("adds auth token as Authorization header", () => {
    const result = buildFetchArgs(
      draft({ authScheme: "Bearer", authToken: "abc123" }),
    );
    expect(result.ok).toBe(true);
    if (result.ok) {
      const headers = result.args.init.headers as Record<string, string>;
      expect(headers["Authorization"]).toBe("Bearer abc123");
    }
  });

  it("supports custom auth schemes", () => {
    const result = buildFetchArgs(
      draft({ authScheme: "Token", authToken: "xyz" }),
    );
    expect(result.ok).toBe(true);
    if (result.ok) {
      const headers = result.args.init.headers as Record<string, string>;
      expect(headers["Authorization"]).toBe("Token xyz");
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
      draft({
        headers: [{ key: "X-Custom", value: "yes", enabled: true }],
      }),
    );
    expect(result.ok).toBe(true);
    if (result.ok) {
      const headers = result.args.init.headers as Record<string, string>;
      expect(headers["X-Custom"]).toBe("yes");
    }
  });

  it("ignores disabled headers", () => {
    const result = buildFetchArgs(
      draft({
        headers: [
          { key: "X-On", value: "yes", enabled: true },
          { key: "X-Off", value: "no", enabled: false },
        ],
      }),
    );
    expect(result.ok).toBe(true);
    if (result.ok) {
      const headers = result.args.init.headers as Record<string, string>;
      expect(headers["X-On"]).toBe("yes");
      expect(headers["X-Off"]).toBeUndefined();
    }
  });
});
