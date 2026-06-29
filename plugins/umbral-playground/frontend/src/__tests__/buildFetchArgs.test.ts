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

  it("strips Content-Type on a body-less GET (avoids empty-JSON 400)", () => {
    const result = buildFetchArgs(
      draft({
        method: "GET",
        url: "/api/crypto_price/get_price_at/",
        headers: [
          { key: "Content-Type", value: "application/json", enabled: true },
        ],
      }),
    );
    expect(result.ok).toBe(true);
    if (result.ok) {
      const headers = result.args.init.headers as Record<string, string>;
      expect(headers["Content-Type"]).toBeUndefined();
      expect(result.args.init.body).toBeUndefined();
    }
  });

  it("keeps Content-Type when a POST actually has a body", () => {
    const result = buildFetchArgs(
      draft({ method: "POST", url: "/api/articles/", body: '{"title":"x"}' }),
    );
    expect(result.ok).toBe(true);
    if (result.ok) {
      const headers = result.args.init.headers as Record<string, string>;
      expect(headers["Content-Type"]).toBe("application/json");
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

  it("applies workspace defaultHeaders to the live request", () => {
    const result = buildFetchArgs(draft(), {
      defaultHeaders: [{ key: "X-Api-Key", value: "abc", enabled: true }],
    });
    expect(result.ok).toBe(true);
    if (result.ok) {
      const headers = result.args.init.headers as Record<string, string>;
      expect(headers["X-Api-Key"]).toBe("abc");
    }
  });

  it("lets a per-request header override a same-named default (case-insensitive)", () => {
    const result = buildFetchArgs(
      draft({ headers: [{ key: "x-api-key", value: "per-request", enabled: true }] }),
      { defaultHeaders: [{ key: "X-Api-Key", value: "default", enabled: true }] },
    );
    expect(result.ok).toBe(true);
    if (result.ok) {
      const headers = result.args.init.headers as Record<string, string>;
      // Exactly one X-Api-Key, with the per-request value.
      const keys = Object.keys(headers).filter(
        (k) => k.toLowerCase() === "x-api-key",
      );
      expect(keys).toEqual(["x-api-key"]);
      expect(headers["x-api-key"]).toBe("per-request");
    }
  });

  it("skips disabled defaultHeaders", () => {
    const result = buildFetchArgs(draft(), {
      defaultHeaders: [{ key: "X-Off", value: "no", enabled: false }],
    });
    expect(result.ok).toBe(true);
    if (result.ok) {
      const headers = result.args.init.headers as Record<string, string>;
      expect(headers["X-Off"]).toBeUndefined();
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

  it("interpolates enabled variables in URL params, headers, auth, and JSON body", () => {
    const result = buildFetchArgs(
      draft({
        method: "POST",
        url: "/api/{{version}}/articles/{id}/",
        params: [
          { key: "id", value: "{{articleId}}", enabled: true },
          { key: "tenant", value: "{{tenant}}", enabled: true },
        ],
        headers: [{ key: "X-Tenant", value: "{{tenant}}", enabled: true }],
        authToken: "{{token}}",
        body: '{"title":"{{title}}"}',
      }),
      {
        variables: [
          { key: "version", value: "v1", enabled: true },
          { key: "articleId", value: "42", enabled: true },
          { key: "tenant", value: "acme", enabled: true },
          { key: "token", value: "abc123", enabled: true },
          { key: "title", value: "Launch", enabled: true },
        ],
      },
    );
    expect(result.ok).toBe(true);
    if (result.ok) {
      const headers = result.args.init.headers as Record<string, string>;
      expect(result.args.url).toBe("/api/v1/articles/42/?tenant=acme");
      expect(headers["X-Tenant"]).toBe("acme");
      expect(headers["Authorization"]).toBe("Bearer abc123");
      expect(result.args.init.body).toBe('{"title":"Launch"}');
    }
  });

  it("leaves disabled or missing variables unchanged", () => {
    const result = buildFetchArgs(
      draft({
        url: "/api/{{disabled}}/{{missing}}/",
      }),
      {
        variables: [{ key: "disabled", value: "v1", enabled: false }],
      },
    );
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.args.url).toBe("/api/{{disabled}}/{{missing}}/");
    }
  });

  it("prefixes relative URLs with a configured base URL", () => {
    const result = buildFetchArgs(draft(), {
      baseUrl: "https://api.example.test/",
    });
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.args.url).toBe("https://api.example.test/api/articles/");
    }
  });

  it("does not prefix absolute URLs with the base URL", () => {
    const result = buildFetchArgs(
      draft({ url: "https://other.example.test/articles/" }),
      { baseUrl: "https://api.example.test" },
    );
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.args.url).toBe("https://other.example.test/articles/");
    }
  });

  it("can include credentials for cookie-backed APIs", () => {
    const result = buildFetchArgs(draft(), { includeCredentials: true });
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.args.init.credentials).toBe("include");
    }
  });
});
