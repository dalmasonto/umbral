/** Pins the multi-language codegen output: JS/TS fetch, cURL,
 *  Python requests, Rust reqwest blocking. The critical
 *  invariants:
 *
 *  - The URL is the FULL resolved URL (base + path + query
 *    string), not the path the user typed into the URL bar.
 *  - Headers in the snippet match the wire (includes the
 *    workspace's default headers, the global auth, and any
 *    per-request overrides).
 *  - Variable interpolation lands in the rendered values.
 *  - Body is the post-interpolation JSON / form data string. */

import { describe, it, expect } from "vitest";
import { generate, snapshotFromRecord } from "../state/codegen";
import type { PlaygroundSettings, ResponseRecord } from "../state/store";

function defaults(over: Partial<PlaygroundSettings> = {}): PlaygroundSettings {
  return {
    baseUrl: "https://api.example.com",
    variables: [],
    defaultHeaders: [
      { key: "Content-Type", value: "application/json", enabled: true },
      { key: "Accept", value: "application/json", enabled: true },
    ],
    includeCredentials: false,
    globalAuth: { enabled: false, scheme: "Bearer", token: "" },
    ...over,
  };
}

function record(overrides: Partial<ResponseRecord["request"]> = {}): ResponseRecord {
  return {
    operationId: "list_product",
    request: {
      method: "GET",
      url: "/api/product/",
      params: [],
      headers: [],
      bodyType: "json",
      body: "",
      formFields: [],
      authScheme: "Bearer",
      authToken: "",
      ...overrides,
    },
    status: 200,
    statusText: "OK",
    durationMs: 12,
    sizeBytes: 0,
    headers: {},
    bodyText: "",
    timestamp: 0,
  };
}

describe("codegen request URL", () => {
  it("uses the full resolved URL (base + path), not the path-only typed URL", () => {
    const r = record();
    const s = snapshotFromRecord(r, defaults())!;
    expect(s.url).toBe("https://api.example.com/api/product/");

    const js = generate("js", s);
    expect(js).toContain('"https://api.example.com/api/product/"');
    const curl = generate("curl", s);
    expect(curl).toContain("'https://api.example.com/api/product/'");
    const py = generate("python", s);
    expect(py).toContain('"https://api.example.com/api/product/"');
    const rust = generate("rust", s);
    expect(rust).toContain('"https://api.example.com/api/product/"');
  });

  it("includes the query string when params are set", () => {
    const r = record({
      params: [
        { key: "fields", value: "id,name,price", enabled: true },
        { key: "page", value: "2", enabled: true },
      ],
    });
    const s = snapshotFromRecord(r, defaults())!;
    expect(s.url).toContain("fields=id%2Cname%2Cprice");
    expect(s.url).toContain("page=2");
  });

  it("interpolates variables into the URL and headers", () => {
    const r = record({
      url: "/api/product/{{slug}}/",
      headers: [{ key: "X-Tenant", value: "tenant-{{tid}}", enabled: true }],
    });
    const s = snapshotFromRecord(
      r,
      defaults({
        variables: [
          { key: "slug", value: "widgets", enabled: true },
          { key: "tid", value: "7", enabled: true },
        ],
      }),
    )!;
    expect(s.url).toBe("https://api.example.com/api/product/widgets/");
    expect(s.headers["X-Tenant"]).toBe("tenant-7");
  });
});

describe("codegen headers", () => {
  it("includes the workspace defaults in every language", () => {
    const r = record();
    const s = snapshotFromRecord(r, defaults())!;
    for (const snippet of [
      generate("js", s),
      generate("curl", s),
      generate("python", s),
      generate("rust", s),
    ]) {
      expect(snippet).toContain("Content-Type");
      expect(snippet).toContain("application/json");
      expect(snippet).toContain("Accept");
    }
  });

  it("applies the global auth header when no per-request token is set", () => {
    const r = record();
    const s = snapshotFromRecord(
      r,
      defaults({
        globalAuth: {
          enabled: true,
          scheme: "Token",
          token: "secret-value",
        },
      }),
    )!;
    expect(s.headers["Authorization"]).toBe("Token secret-value");
    expect(generate("js", s)).toContain('"Authorization": "Token secret-value"');
    expect(generate("curl", s)).toContain("Authorization: Token secret-value");
    expect(generate("python", s)).toContain('"Authorization": "Token secret-value"');
    expect(generate("rust", s)).toMatch(/"Authorization"/);
    expect(generate("rust", s)).toContain('"Token secret-value"');
  });

  it("per-request auth always wins over global auth", () => {
    const r = record({ authScheme: "Bearer", authToken: "request-level" });
    const s = snapshotFromRecord(
      r,
      defaults({
        globalAuth: {
          enabled: true,
          scheme: "Token",
          token: "global-level",
        },
      }),
    )!;
    expect(s.headers["Authorization"]).toBe("Bearer request-level");
  });
});

describe("codegen body", () => {
  it("includes the JSON body for POST", () => {
    const r = record({
      method: "POST",
      url: "/api/product/",
      body: '{"name":"Widget"}',
    });
    const s = snapshotFromRecord(r, defaults())!;
    // JS uses unquoted object-literal keys (`body: "..."`).
    expect(generate("js", s)).toContain('body: "{\\"name\\":\\"Widget\\"}"');
    expect(generate("curl", s)).toContain("--data-raw '{\"name\":\"Widget\"}'");
    expect(generate("python", s)).toContain("data = ");
    expect(generate("rust", s)).toContain(".body(");
  });

  it("does not include a body for GET", () => {
    const r = record();
    const s = snapshotFromRecord(r, defaults())!;
    expect(generate("js", s)).not.toContain("body:");
    expect(generate("rust", s)).not.toContain(".body(");
  });
});

describe("codegen method dispatch", () => {
  it("uses the right reqwest method fn in Rust", () => {
    const methods = ["GET", "POST", "PUT", "PATCH", "DELETE"];
    const reqwestFns: Record<string, string> = {
      GET: ".get(",
      POST: ".post(",
      PUT: ".put(",
      PATCH: ".patch(",
      DELETE: ".delete(",
    };
    for (const m of methods) {
      const r = record({ method: m, body: m === "GET" ? "" : "{}" });
      const s = snapshotFromRecord(r, defaults())!;
      const rust = generate("rust", s);
      expect(rust).toContain(reqwestFns[m]);
    }
  });

  it("uses the right verb in cURL and JS for each method", () => {
    for (const m of ["GET", "POST", "PATCH", "DELETE"]) {
      const r = record({ method: m, body: m === "GET" ? "" : "{}" });
      const s = snapshotFromRecord(r, defaults())!;
      expect(generate("curl", s)).toContain(`curl -X ${m}`);
      // JS object-literal keys are unquoted (`method: "GET",`).
      expect(generate("js", s)).toContain(`method: "${m}"`);
    }
  });
});
