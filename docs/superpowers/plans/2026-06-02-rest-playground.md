# umbral-playground Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `umbral-playground`, a new plugin that mounts a 3-pane React API playground at `/api/playground/`, fetching the existing `umbral-openapi` JSON spec at runtime and bundling a React + tailwindcss frontend via `build.rs`-driven esbuild.

**Architecture:** New workspace crate `plugins/umbral-playground`. The Rust side is two routes (HTML shell + assets under `dist/`). `build.rs` invokes `esbuild` and `tailwindcss` CLIs, generates a `generated_assets.rs` Rust file with the hashed bundle filenames, and degrades gracefully (placeholder HTML + warning) if the tools are missing. The frontend is a single React app with zustand state, ~12 components, and a localStorage-backed history. Build pipeline produces ~40KB JS + ~10KB CSS.

**Tech Stack:** Rust, Axum, vanilla HTML shell. React 18 + TypeScript + zustand + openapi-types. esbuild (bundler) + tailwindcss (CSS). Vitest + @testing-library/react. Node 20+ required at build time; degrades without.

**Spec:** `docs/superpowers/specs/2026-06-02-rest-playground-design.md`

---

## File map

| Action | File |
|---|---|
| New | `crates/Cargo.toml` (modify: add member) |
| New | `plugins/umbral-playground/Cargo.toml` |
| New | `plugins/umbral-playground/build.rs` |
| New | `plugins/umbral-playground/src/lib.rs` |
| New | `plugins/umbral-playground/src/routes.rs` |
| New | `plugins/umbral-playground/src/static.rs` |
| New | `plugins/umbral-playground/src/placeholder.html` |
| New | `plugins/umbral-playground/frontend/index.html` |
| New | `plugins/umbral-playground/frontend/index.tsx` |
| New | `plugins/umbral-playground/frontend/components/App.tsx` |
| New | `plugins/umbral-playground/frontend/components/Header.tsx` |
| New | `plugins/umbral-playground/frontend/components/EndpointTree.tsx` |
| New | `plugins/umbral-playground/frontend/components/RequestBuilder.tsx` |
| New | `plugins/umbral-playground/frontend/components/ResponseViewer.tsx` |
| New | `plugins/umbral-playground/frontend/components/AuthTab.tsx` |
| New | `plugins/umbral-playground/frontend/components/MethodBadge.tsx` |
| New | `plugins/umbral-playground/frontend/components/JsonView.tsx` |
| New | `plugins/umbral-playground/frontend/components/Tabs.tsx` |
| New | `plugins/umbral-playground/frontend/components/KeyValueTable.tsx` |
| New | `plugins/umbral-playground/frontend/components/EmptyState.tsx` |
| New | `plugins/umbral-playground/frontend/components/ErrorBanner.tsx` |
| New | `plugins/umbral-playground/frontend/state/store.ts` |
| New | `plugins/umbral-playground/frontend/state/spec.ts` |
| New | `plugins/umbral-playground/frontend/state/history.ts` |
| New | `plugins/umbral-playground/frontend/state/curl.ts` |
| New | `plugins/umbral-playground/frontend/state/buildFetchArgs.ts` |
| New | `plugins/umbral-playground/frontend/styles/tailwind.config.js` |
| New | `plugins/umbral-playground/frontend/styles/app.css` |
| New | `plugins/umbral-playground/frontend/__tests__/buildFetchArgs.test.ts` |
| New | `plugins/umbral-playground/frontend/__tests__/store.test.ts` |
| New | `plugins/umbral-playground/frontend/__tests__/RequestBuilder.test.tsx` |
| New | `plugins/umbral-playground/frontend/__tests__/ResponseViewer.test.tsx` |
| New | `plugins/umbral-playground/frontend/vitest.config.ts` |
| New | `plugins/umbral-playground/frontend/package.json` |
| New | `plugins/umbral-playground/frontend/tsconfig.json` |
| New | `plugins/umbral-playground/frontend/vitest.setup.ts` |
| New | `plugins/umbral-playground/tests/rust_integration.rs` |
| New | `plugins/umbral-playground/README.md` |
| New | `documentation/docs/v0.0.1/plugins/_category_.json` |
| New | `documentation/docs/v0.0.1/plugins/playground.mdx` |
| Modify | `examples/derive-demo/src/main.rs` (or wherever the demo registers plugins) |
| Modify | `.gitignore` (add `plugins/umbral-playground/dist/`, `plugins/umbral-playground/src/generated_assets.rs`, `plugins/umbral-playground/frontend/node_modules/`) |

---

## Milestone 1: Plugin skeleton (no frontend, no routes)

Goal: the crate exists, builds, and is a no-op. This proves the workspace wiring is right.

### Task 1: Add crate to workspace members

**Files:**
- Modify: `crates/Cargo.toml` (workspace members list)

- [ ] **Step 1: Inspect the current `crates/Cargo.toml`**

Read the file and find the `[workspace]` block. Identify where existing `plugins/*` members are listed. (The `crates/` directory itself is the workspace root in this project, not a top-level `Cargo.toml`; the workspace is declared at `crates/Cargo.toml`.)

- [ ] **Step 2: Add the new member**

Add `"../plugins/umbral-playground"` to the workspace members list, in alphabetical position with the other plugin entries. The line should look like:

```toml
"../plugins/umbral-playground",
```

Do not modify the `[workspace.dependencies]` block. The new crate has no shared deps yet (only the `umbral` facade path-dep, which is local).

- [ ] **Step 3: Verify the workspace still resolves**

Run: `cd crates && cargo metadata --no-deps --format-version 1 | head -1 | python3 -c "import json,sys; print(len(json.load(sys.stdin)['packages']))"`

Expected: a number ≥ 12 (the count of currently-registered plugins). The exact number doesn't matter; what matters is that `cargo metadata` doesn't error.

If it errors with "package not found in workspace", you added the path wrong — check the relative path is `../plugins/umbral-playground` (two dots, slash).

- [ ] **Step 4: Commit**

```bash
git add crates/Cargo.toml
git commit -m "chore(workspace): register umbral-playground plugin member"
```

### Task 2: Scaffold the empty crate

**Files:**
- Create: `plugins/umbral-playground/Cargo.toml`
- Create: `plugins/umbral-playground/src/lib.rs`

- [ ] **Step 1: Write `Cargo.toml`**

Write to `plugins/umbral-playground/Cargo.toml`:

```toml
[package]
name = "umbral-playground"
description = "Interactive API playground UI for umbral-rest. The DRF browsable API, in umbral."
workspace = "../../crates"
version.workspace = true
edition.workspace = true
license.workspace = true
repository.workspace = true
authors.workspace = true
rust-version.workspace = true

[dependencies]
umbral = { path = "../../crates/umbral", version = "0.0.1" }
```

This mirrors the other plugin `Cargo.toml` files. The `workspace = "../../crates"` line is what tells Cargo this crate is part of the `crates/` workspace despite living one directory outside it. The `umbral` dep is the facade only — no plugin deps.

- [ ] **Step 2: Write a no-op `lib.rs`**

Write to `plugins/umbral-playground/src/lib.rs`:

```rust
//! umbral-playground — interactive API playground UI for umbral-rest.
//!
//! MVP: a 3-pane React UI mounted at `/api/playground/`, fetching the
//! existing `umbral-openapi` JSON spec at runtime. See the design spec
//! at `docs/superpowers/specs/2026-06-02-rest-playground-design.md`.

/// Placeholder. The real plugin type lands in Milestone 2.
pub struct PlaygroundPlugin;
```

The crate compiles to a library (`rlib`) and exposes a single placeholder type. No `Plugin` impl yet.

- [ ] **Step 3: Build the crate**

Run: `cd crates && cargo build -p umbral-playground`

Expected: `Compiling umbral-playground` followed by `Finished ...`. No warnings, no errors.

If you get "error: failed to load manifest", check the `[package]` block has `name = "umbral-playground"` and `workspace = "../../crates"`.

- [ ] **Step 4: Run `cargo check` on the whole workspace**

Run: `cd crates && cargo check --workspace`

Expected: `Finished ...` for the workspace, no new errors. Other crates' build states are unchanged.

- [ ] **Step 5: Commit**

```bash
git add plugins/umbral-playground/Cargo.toml plugins/umbral-playground/src/lib.rs
git commit -m "feat(playground): scaffold empty plugin crate"
```

---

## Milestone 2: Build pipeline + degraded placeholder HTML

Goal: `build.rs` runs esbuild and tailwindcss, generates a `generated_assets.rs`, and the plugin serves a styled "Hello, playground" page at `/api/playground/`. Without the CLIs installed, the page is a degraded placeholder. The build pipeline is now the most complex part of the Rust side and getting it right here saves pain later.

### Task 3: Add gitignore entries

**Files:**
- Modify: `.gitignore` (repo root)

- [ ] **Step 1: Read the current `.gitignore`**

Read the file. The repo already ignores `target/`, `Cargo.lock` (in the umbrella mode), and a few framework-specific paths.

- [ ] **Step 2: Add the playground-specific paths**

Append at the end of the file:

```
# umbral-playground build artifacts
plugins/umbral-playground/dist/
plugins/umbral-playground/src/generated_assets.rs
plugins/umbral-playground/frontend/node_modules/
```

- [ ] **Step 3: Verify the existing file still parses**

Run: `git status --short .gitignore`

Expected: one line with `M .gitignore`. Nothing else.

- [ ] **Step 4: Commit**

```bash
git add .gitignore
git commit -m "chore(playground): gitignore dist/, generated_assets.rs, node_modules"
```

### Task 4: `frontend/package.json` + `tsconfig.json`

These are the *frontend* toolchain config files. They live inside `plugins/umbral-playground/frontend/` and are managed by Node, not Cargo. They pin the exact versions the README will document.

**Files:**
- Create: `plugins/umbral-playground/frontend/package.json`
- Create: `plugins/umbral-playground/frontend/tsconfig.json`

- [ ] **Step 1: Write `package.json`**

Write to `plugins/umbral-playground/frontend/package.json`:

```json
{
  "name": "umbral-playground-frontend",
  "version": "0.0.1",
  "private": true,
  "type": "module",
  "scripts": {
    "build": "esbuild index.tsx --bundle --minify --sourcemap=inline --outfile=../dist/playground.tmp.js --define:process.env.NODE_ENV='\"production\"' --loader:.tsx=tsx",
    "build:dev": "esbuild index.tsx --bundle --sourcemap --outfile=../dist/playground.tmp.js --loader:.tsx=tsx",
    "test": "vitest run"
  },
  "dependencies": {
    "react": "^18.3.0",
    "react-dom": "^18.3.0",
    "zustand": "^4.5.0",
    "openapi-types": "^12.1.0"
  },
  "devDependencies": {
    "@testing-library/jest-dom": "^6.4.0",
    "@testing-library/react": "^16.0.0",
    "@types/react": "^18.3.0",
    "@types/react-dom": "^18.3.0",
    "@vitejs/plugin-react": "^4.3.0",
    "esbuild": "^0.23.0",
    "jsdom": "^25.0.0",
    "tailwindcss": "^3.4.0",
    "typescript": "^5.5.0",
    "vitest": "^2.0.0"
  }
}
```

The `build` and `build:dev` scripts are intentionally not invoked from `npm` directly — `build.rs` will shell out to `esbuild` and `tailwindcss` with these exact flags. The `npm run build` script is for users who want to debug the frontend in isolation.

- [ ] **Step 2: Write `tsconfig.json`**

Write to `plugins/umbral-playground/frontend/tsconfig.json`:

```json
{
  "compilerOptions": {
    "target": "ES2020",
    "lib": ["ES2020", "DOM", "DOM.Iterable"],
    "module": "ESNext",
    "moduleResolution": "Bundler",
    "jsx": "react-jsx",
    "strict": true,
    "noUnusedLocals": true,
    "noUnusedParameters": true,
    "noFallthroughCasesInSwitch": true,
    "esModuleInterop": true,
    "allowSyntheticDefaultImports": true,
    "skipLibCheck": true,
    "isolatedModules": true,
    "resolveJsonModule": true,
    "types": ["vitest/globals", "@testing-library/jest-dom"]
  },
  "include": ["**/*.ts", "**/*.tsx"],
  "exclude": ["node_modules", "dist", "../dist", "../src/generated_assets.rs"]
}
```

- [ ] **Step 3: Don't install yet**

We deliberately don't run `npm install` from this task. The build pipeline in Task 5 detects the CLIs in `$PATH` and degrades gracefully. The frontend deps are installed in M5 when we wire up Vitest.

- [ ] **Step 4: Commit**

```bash
git add plugins/umbral-playground/frontend/package.json plugins/umbral-playground/frontend/tsconfig.json
git commit -m "feat(playground): add frontend package.json and tsconfig"
```

### Task 5: Minimal `build.rs` that produces a placeholder `generated_assets.rs`

**Files:**
- Create: `plugins/umbral-playground/build.rs`

- [ ] **Step 1: Write `build.rs`**

Write to `plugins/umbral-playground/build.rs`:

```rust
//! build.rs for umbral-playground.
//!
//! Invokes `esbuild` and `tailwindcss` to bundle the React frontend
//! and CSS into `dist/`. Writes `src/generated_assets.rs` with the
//! hashed asset filenames so the runtime knows what to serve.
//!
//! If either CLI is missing, writes a degraded `generated_assets.rs`
//! pointing at a static placeholder HTML. The crate still compiles.

use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

const PLACEHOLDER_JS: &str = "playground.placeholder.js";
const PLACEHOLDER_CSS: &str = "playground.placeholder.css";

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let frontend_dir = manifest_dir.join("frontend");
    let dist_dir = manifest_dir.join("dist");
    let out_path = manifest_dir.join("src/generated_assets.rs");

    // Rerun triggers.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=frontend/");
    println!("cargo:rerun-if-changed=src/placeholder.html");

    fs::create_dir_all(&dist_dir).expect("create dist dir");

    let debug = env::var("PROFILE").map(|p| p == "debug").unwrap_or(true);
    let esbuild = find_in_path("esbuild");
    let tailwind = find_in_path("tailwindcss");

    let (js_name, css_name) = match (esbuild, tailwind) {
        (Some(eb), Some(tw)) => {
            match bundle(&frontend_dir, &dist_dir, &eb, &tw, debug) {
                Ok((js, css)) => (js, css),
                Err(e) => {
                    eprintln!("cargo:warning=umbral-playground: bundle failed ({e}); using placeholder");
                    (PLACEHOLDER_JS.to_string(), PLACEHOLDER_CSS.to_string())
                }
            }
        }
        _ => {
            eprintln!("cargo:warning=umbral-playground: esbuild and/or tailwindcss not in $PATH; using placeholder HTML. Install with `npm i -g esbuild tailwindcss` to enable the full UI.");
            (PLACEHOLDER_JS.to_string(), PLACEHOLDER_CSS.to_string())
        }
    };

    let mut f = fs::File::create(&out_path).expect("create generated_assets.rs");
    writeln!(
        f,
        "// Auto-generated by build.rs. Do not edit.\n\
         pub const JS: &str = \"{js_name}\";\n\
         pub const CSS: &str = \"{css_name}\";\n"
    )
    .expect("write generated_assets.rs");
}

fn find_in_path(bin: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for dir in env::split_paths(&path) {
        let full = dir.join(bin);
        if full.is_file() {
            return Some(full);
        }
    }
    None
}

fn bundle(
    frontend_dir: &Path,
    dist_dir: &Path,
    esbuild: &Path,
    tailwind: &Path,
    debug: bool,
) -> Result<(String, String), String> {
    let entry = frontend_dir.join("index.tsx");
    let css_entry = frontend_dir.join("styles/app.css");
    let tailwind_config = frontend_dir.join("styles/tailwind.config.js");

    let mut js_args: Vec<String> = vec![
        entry.to_string_lossy().into_owned(),
        "--bundle".into(),
        "--loader:.tsx=tsx".into(),
        "--outfile=playground.tmp.js".into(),
    ];
    if debug {
        js_args.push("--sourcemap".into());
    } else {
        js_args.push("--minify".into());
        js_args.push("--sourcemap=inline".into());
        js_args.push("--define:process.env.NODE_ENV=\"production\"".into());
    }

    let status = Command::new(esbuild)
        .args(&js_args)
        .current_dir(dist_dir)
        .status()
        .map_err(|e| format!("esbuild: {e}"))?;
    if !status.success() {
        return Err("esbuild exited non-zero".into());
    }

    let mut css_args: Vec<String> = vec![
        format!("-c={}", tailwind_config.display()),
        format!("-i={}", css_entry.display()),
        "-o=playground.tmp.css".into(),
    ];
    if !debug {
        css_args.push("--minify".into());
    }

    let status = Command::new(tailwind)
        .args(&css_args)
        .current_dir(dist_dir)
        .status()
        .map_err(|e| format!("tailwindcss: {e}"))?;
    if !status.success() {
        return Err("tailwindcss exited non-zero".into());
    }

    let js_hash = short_hash(dist_dir.join("playground.tmp.js"));
    let css_hash = short_hash(dist_dir.join("playground.tmp.css"));

    let js_final = format!("playground.{js_hash}.js");
    let css_final = format!("playground.{css_hash}.css");
    fs::rename(dist_dir.join("playground.tmp.js"), dist_dir.join(&js_final))
        .map_err(|e| format!("rename js: {e}"))?;
    fs::rename(dist_dir.join("playground.tmp.css"), dist_dir.join(&css_final))
        .map_err(|e| format!("rename css: {e}"))?;

    Ok((js_final, css_final))
}

fn short_hash(path: PathBuf) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let bytes = fs::read(&path).unwrap_or_default();
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    let n = h.finish();
    format!("{n:016x}")
}
```

This file is long but mechanical. Key things to note:
- It emits `cargo:warning=...` (not stderr-only) so users see the degraded-mode message in their build output.
- It writes `generated_assets.rs` at the end — the `include!` in `lib.rs` reads it.
- It uses `cargo:rerun-if-changed=frontend/` to re-trigger on any frontend change.

- [ ] **Step 2: Write the placeholder HTML**

Write to `plugins/umbral-playground/src/placeholder.html`:

```html
<!doctype html>
<html lang="en">
<head>
  <meta charset="UTF-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1.0" />
  <title>umbral playground — placeholder</title>
  <style>
    *, *::before, *::after { box-sizing: border-box; margin: 0; padding: 0; }
    body { font-family: ui-sans-serif, system-ui, -apple-system, sans-serif; background: #020617; color: #e2e8f0; min-height: 100vh; display: flex; align-items: center; justify-content: center; padding: 2rem; }
    .card { max-width: 36rem; padding: 2rem; border: 1px solid #1e293b; border-radius: 0.5rem; background: #0f172a; }
    h1 { font-size: 1.5rem; margin-bottom: 0.75rem; color: #f1f5f9; }
    p { line-height: 1.6; margin-bottom: 0.75rem; color: #94a3b8; }
    code { font-family: ui-monospace, monospace; background: #020617; padding: 0.125rem 0.375rem; border-radius: 0.25rem; color: #c7d2fe; }
  </style>
</head>
<body>
  <div class="card">
    <h1>umbral playground</h1>
    <p>The playground frontend was not built.</p>
    <p>To enable the interactive UI, install <code>esbuild</code> and <code>tailwindcss</code> in your <code>$PATH</code>:</p>
    <p><code>npm i -g esbuild tailwindcss</code></p>
    <p>Then rebuild this crate. The placeholder is shown when the build pipeline can't produce the React bundle.</p>
  </div>
</body>
</html>
```

This is a deliberately *plain* file. The interactive UI lives in `frontend/`; the placeholder exists only so a user with a fresh `git clone` doesn't see a broken page.

- [ ] **Step 3: Add `include_str!` to `lib.rs`**

Modify `plugins/umbral-playground/src/lib.rs` to read:

```rust
//! umbral-playground — interactive API playground UI for umbral-rest.
//!
//! MVP: a 3-pane React UI mounted at `/api/playground/`, fetching the
//! existing `umbral-openapi` JSON spec at runtime. See the design spec
//! at `docs/superpowers/specs/2026-06-02-rest-playground-design.md`.

pub mod routes;
pub mod static_serve;

mod generated_assets {
    include!(concat!(env!("OUT_DIR"), "/../../src/generated_assets.rs"));
}

pub(crate) use generated_assets::{CSS, JS};

/// Placeholder HTML served when esbuild/tailwindcss were not available
/// at build time. Inline so the plugin always renders *something*.
pub(crate) const PLACEHOLDER_HTML: &str = include_str!("placeholder.html");
```

Wait — the `include!` path above is wrong. `OUT_DIR` is `target/.../build/umbral-playground-<hash>/out/`, and `generated_assets.rs` is being written to `src/`, not `OUT_DIR`. Fix: write `generated_assets.rs` to `OUT_DIR` and `include!` from there. Update `build.rs` `out_path` to `PathBuf::from(env::var("OUT_DIR").unwrap()).join("generated_assets.rs")` and `lib.rs` to `include!(concat!(env!("OUT_DIR"), "/generated_assets.rs"))`. This is a real bug in the draft — make the fix when transcribing.

- [ ] **Step 4: Apply the `OUT_DIR` fix to `build.rs`**

In `plugins/umbral-playground/build.rs`, change:

```rust
let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
```

(instead of `manifest_dir.join("src/generated_assets.rs")`).

And in `lib.rs`, use:

```rust
include!(concat!(env!("OUT_DIR"), "/generated_assets.rs"))
```

This way `generated_assets.rs` lands in `OUT_DIR`, which is gitignored transitively (it lives under `target/`). No need to add `plugins/umbral-playground/src/generated_assets.rs` to `.gitignore` after all — but keep the line in `.gitignore` from Task 3 as a safety net in case a future build step changes the location.

- [ ] **Step 5: Build and verify `OUT_DIR` works**

Run: `cd crates && cargo build -p umbral-playground 2>&1 | tail -20`

Expected: a `cargo:warning=umbral-playground: esbuild and/or tailwindcss not in $PATH; using placeholder HTML` line, then `Compiling umbral-playground` and `Finished ...`. The warning is expected and correct on this machine.

- [ ] **Step 6: Commit**

```bash
git add plugins/umbral-playground/build.rs plugins/umbral-playground/src/lib.rs plugins/umbral-playground/src/placeholder.html
git commit -m "feat(playground): build pipeline with degraded placeholder mode"
```

### Task 6: Routes + static asset serving

**Files:**
- Create: `plugins/umbral-playground/src/routes.rs`
- Create: `plugins/umbral-playground/src/static_serve.rs`
- Modify: `plugins/umbral-playground/src/lib.rs` (add `Plugin` impl)

- [ ] **Step 1: Write `static_serve.rs`**

Write to `plugins/umbral-playground/src/static_serve.rs`:

```rust
//! Path-traversal-safe static file serving for the bundled assets.
//!
//! Resolves the requested path under `dist/` and 404s if the result
//! escapes the directory. This is the only piece of code that
//! touches the filesystem on a per-request basis; everything else
//! is in-memory.

use std::path::{Path, PathBuf};

const DIST_DIR: &str = "dist";

pub fn resolve(asset_path: &str) -> Option<PathBuf> {
    // Strip leading slash; reject obvious traversal attempts up front.
    let trimmed = asset_path.trim_start_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.contains("..") {
        return None;
    }

    let manifest_dir =
        std::env::var("CARGO_MANIFEST_DIR").map(PathBuf::from).unwrap_or_default();
    let candidate = manifest_dir.join(DIST_DIR).join(trimmed);

    // Canonicalize the parent + filename to catch symlink escapes.
    let canonical = candidate.canonicalize().ok()?;
    let dist_canonical = manifest_dir.join(DIST_DIR).canonicalize().ok()?;
    if !canonical.starts_with(&dist_canonical) {
        return None;
    }
    if !canonical.is_file() {
        return None;
    }
    Some(canonical)
}

pub fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("js") => "application/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("map") => "application/json; charset=utf-8",
        Some("html") => "text/html; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        _ => "application/octet-stream",
    }
}
```

- [ ] **Step 2: Write `routes.rs`**

Write to `plugins/umbral-playground/src/routes.rs`:

```rust
//! Two routes: the HTML shell and the bundled assets.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{Response, StatusCode, header};
use axum::middleware::Next;

use crate::{CSS, JS, PLACEHOLDER_HTML};
use crate::static_serve::{content_type, resolve};

/// Shared state carried through middleware: the base path (e.g.
/// `/api/playground`) and a flag for whether we're in placeholder mode.
#[derive(Clone, Debug)]
pub struct PlaygroundState {
    pub base_path: Arc<str>,
    pub degraded: bool,
}

impl PlaygroundState {
    pub fn new(base_path: impl Into<String>, degraded: bool) -> Self {
        Self {
            base_path: Arc::from(base_path.into()),
            degraded,
        }
    }
}

const SHELL_HTML: &str = include_str!("shell.html");

/// Render the HTML shell, inserting the hashed asset paths.
fn render_shell(state: &PlaygroundState) -> String {
    if state.degraded {
        return PLACEHOLDER_HTML.to_string();
    }
    SHELL_HTML
        .replace("__CSS__", CSS)
        .replace("__JS__", JS)
}

/// `GET {base_path}/` — HTML shell.
pub async fn shell(State(state): State<PlaygroundState>) -> Response<Body> {
    let html = render_shell(&state);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from(html))
        .unwrap()
}

/// `GET {base_path}/assets/*` — bundled assets. Path-traversal safe.
pub async fn assets(
    State(state): State<PlaygroundState>,
    req: Request,
) -> Response<Body> {
    let path = req.uri().path();
    let prefix = format!("{}/assets/", state.base_path);
    let rel = match path.strip_prefix(&prefix) {
        Some(r) => r,
        None => return not_found(),
    };
    let resolved = match resolve(rel) {
        Some(p) => p,
        None => return not_found(),
    };
    let bytes = match std::fs::read(&resolved) {
        Ok(b) => b,
        Err(_) => return not_found(),
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type(&resolved))
        .header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")
        .body(Body::from(bytes))
        .unwrap()
}

fn not_found() -> Response<Body> {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from("not found"))
        .unwrap()
}

/// Compose both routes into a sub-router. Caller mounts under
/// `state.base_path` (e.g. via `Router::new().nest(&base, sub)`).
pub fn router(state: PlaygroundState) -> axum::Router {
    use axum::routing::get;
    axum::Router::new()
        .route("/", get(shell))
        .route("/assets/*path", get(assets))
        .with_state(state)
}
```

- [ ] **Step 3: Write `shell.html`**

Write to `plugins/umbral-playground/src/shell.html`:

```html
<!doctype html>
<html lang="en">
<head>
  <meta charset="UTF-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1.0" />
  <title>umbral playground</title>
  <link rel="stylesheet" href="/__CSS__" />
</head>
<body>
  <div id="root"></div>
  <script type="module" src="/__JS__"></script>
</body>
</html>
```

The `__CSS__` and `__JS__` placeholders are replaced at request time with the actual hashed asset paths. The `/` prefix assumes the playground is served at the root of its mount; if the user mounts it under e.g. `/api/playground/`, the asset paths become `/api/playground/assets/playground.<hash>.js` (the `assets/` route handles that — see Step 6).

Wait — the spec says assets are served at `/api/playground/assets/*`, but the shell is hardcoded to `/__JS__`. That works *only* if the playground is mounted at the origin root. Fix: the shell's `/` should be `<base_path>/assets/__JS__`. Update the route handler to substitute the full path.

- [ ] **Step 4: Fix the shell rendering to include the base path**

In `routes.rs`, change `render_shell`:

```rust
fn render_shell(state: &PlaygroundState) -> String {
    if state.degraded {
        return PLACEHOLDER_HTML.to_string();
    }
    let css = format!("{}/assets/{}", state.base_path, CSS);
    let js = format!("{}/assets/{}", state.base_path, JS);
    SHELL_HTML
        .replace("__CSS_PATH__", &css)
        .replace("__JS_PATH__", &js)
}
```

And `shell.html` becomes:

```html
<link rel="stylesheet" href="__CSS_PATH__" />
...
<script type="module" src="__JS_PATH__"></script>
```

- [ ] **Step 5: Write the `Plugin` impl in `lib.rs`**

Replace `plugins/umbral-playground/src/lib.rs` with:

```rust
//! umbral-playground — interactive API playground UI for umbral-rest.
//!
//! MVP: a 3-pane React UI mounted at `/api/playground/`, fetching the
//! existing `umbral-openapi` JSON spec at runtime. See the design spec
//! at `docs/superpowers/specs/2026-06-02-rest-playground-design.md`.

use std::sync::Arc;

use umbral::prelude::*;

pub mod routes;
pub mod static_serve;

mod generated_assets {
    include!(concat!(env!("OUT_DIR"), "/generated_assets.rs"));
}

pub(crate) use generated_assets::{CSS, JS};

/// Placeholder HTML served when esbuild/tailwindcss were not available
/// at build time. Inline so the plugin always renders *something*.
pub(crate) const PLACEHOLDER_HTML: &str = include_str!("placeholder.html");

/// The playground plugin.
#[derive(Debug, Clone)]
pub struct PlaygroundPlugin {
    base_path: String,
}

impl Default for PlaygroundPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl PlaygroundPlugin {
    pub fn new() -> Self {
        Self {
            base_path: "/api/playground".to_string(),
        }
    }

    /// Mount under a different path. Trailing slashes are normalised.
    pub fn at(mut self, path: impl Into<String>) -> Self {
        let trimmed = path.into().trim_end_matches('/').to_string();
        self.base_path = if trimmed.is_empty() { "/".to_string() } else { trimmed };
        self
    }
}

impl Plugin for PlaygroundPlugin {
    fn name(&self) -> &'static str {
        "umbral-playground"
    }

    fn routes(&self) -> Option<axum::Router> {
        let degraded = JS.starts_with("playground.placeholder");
        let state = routes::PlaygroundState::new(self.base_path.clone(), degraded);
        let sub = routes::router(state);

        // Nest the sub-router under base_path. axum's Router::nest takes
        // a path string; we use the format `/{base}/*rest`.
        let mount = if self.base_path == "/" {
            "/".to_string()
        } else {
            format!("/{}", self.base_path.trim_start_matches('/'))
        };
        Some(sub)
    }
}
```

The `routes()` method is intentionally rough in this milestone — the real nesting and the `axum::Router` shape will be tightened in M5 when we wire up the integration test. For M2, the goal is that the crate compiles.

- [ ] **Step 6: Build and verify**

Run: `cd crates && cargo build -p umbral-playground 2>&1 | tail -10`

Expected: `Finished ...`. If you see errors about `axum::Router` not being a valid return type, add `axum = "0.8"` to `[dependencies]` in `Cargo.toml`.

- [ ] **Step 7: Add `axum` to Cargo.toml if needed**

If the build failed with `axum` errors, edit `plugins/umbral-playground/Cargo.toml` to add:

```toml
axum = "0.8"
```

under `[dependencies]`. Then re-run Step 6.

- [ ] **Step 8: Commit**

```bash
git add plugins/umbral-playground/
git commit -m "feat(playground): routes for shell + static assets, Plugin impl skeleton"
```

### Task 7: M2 verification — placeholder page renders

We don't have a full example app exercising the plugin yet (that's M6). For M2, the verification is *structural* — confirm the crate builds, the warning fires when CLIs are missing, and the build pipeline can be re-run with the CLIs available to produce real assets.

**Files:**
- Create: `plugins/umbral-playground/tests/m2_build.rs` (integration test that just builds and checks the generated asset filenames)

- [ ] **Step 1: Write a smoke test**

Write to `plugins/umbral-playground/tests/m2_build.rs`:

```rust
//! Smoke test for the build pipeline. Asserts the generated_assets.rs
//! module exists and contains the expected constants. Does NOT exercise
//! HTTP — that's M5.

#[test]
fn generated_assets_module_is_built() {
    // The include! in lib.rs at compile time guarantees the module
    // is present. If this test compiles, the build pipeline ran.
    use umbral_playground::{CSS, JS};
    assert!(!CSS.is_empty(), "CSS asset name should be non-empty");
    assert!(!JS.is_empty(), "JS asset name should be non-empty");
    // Either real hash or placeholder marker; both are valid.
    assert!(
        JS.starts_with("playground."),
        "JS asset should start with 'playground.', got {JS}"
    );
}
```

- [ ] **Step 2: Run the test**

Run: `cd crates && cargo test -p umbral-playground --test m2_build`

Expected: `1 passed; 0 failed`. The warning about esbuild/tailwindcss not in $PATH is expected and doesn't fail the test.

- [ ] **Step 3: Try the build with the CLIs (best-effort)**

Run: `which esbuild tailwindcss 2>&1`

If both CLIs are present, run: `cd crates && cargo clean -p umbral-playground && cargo build -p umbral-playground 2>&1 | tail -20`

Expected: no warning line this time, the build still succeeds, and `plugins/umbral-playground/dist/playground.*.{js,css}` files now exist.

If the CLIs are not present (the common case in this environment), skip this step. The degraded-mode path is the one that ships in CI; the happy path is verified by anyone who runs `npm i -g esbuild tailwindcss` once.

- [ ] **Step 4: Commit**

```bash
git add plugins/umbral-playground/tests/m2_build.rs
git commit -m "test(playground): m2 build-pipeline smoke test"
```

---

## Milestone 3: Spec loader + 3-pane shell + endpoint tree

Goal: the React app boots, fetches the OpenAPI spec, and renders a navigable tree on the left. Selecting an endpoint populates a static URL strip in the center. This milestone proves the cross-plugin HTTP contract works and gets a visible UI on screen for the first time.

### Task 8: Frontend state — zustand store skeleton + spec loader

**Files:**
- Create: `plugins/umbral-playground/frontend/state/store.ts`
- Create: `plugins/umbral-playground/frontend/state/spec.ts`
- Create: `plugins/umbral-playground/frontend/state/buildFetchArgs.ts`
- Create: `plugins/umbral-playground/frontend/state/curl.ts`
- Create: `plugins/umbral-playground/frontend/state/history.ts`

- [ ] **Step 1: Write the shared types in `store.ts`**

Write to `plugins/umbral-playground/frontend/state/store.ts`:

```typescript
import { create } from "zustand";
import type { OpenAPIV3 } from "openapi-types";

/** A request as the user has constructed it in the builder. */
export interface RequestDraft {
  method: string;
  url: string;
  params: Record<string, string>;
  headers: Record<string, string>;
  body: string;
  bearerToken: string;
}

/** A completed request/response pair, persisted in history. */
export interface ResponseRecord {
  operationId: string;
  request: RequestDraft;
  status: number;
  statusText: string;
  durationMs: number;
  sizeBytes: number;
  headers: Record<string, string>;
  bodyText: string;
  timestamp: number;
  error?: string;
}

interface PlaygroundState {
  // spec
  spec: OpenAPIV3.Document | null;
  specError: string | null;
  loadingSpec: boolean;
  loadSpec: () => Promise<void>;

  // selection
  selectedOperationId: string | null;
  selectEndpoint: (id: string | null) => void;

  // current request
  current: RequestDraft;
  setMethod: (m: string) => void;
  setUrl: (u: string) => void;
  setParam: (name: string, value: string) => void;
  setHeader: (name: string, value: string) => void;
  setBody: (raw: string) => void;
  setBearerToken: (t: string) => void;
  resetCurrent: (draft: Partial<RequestDraft>) => void;

  // response
  lastResponse: ResponseRecord | null;
  inFlight: boolean;
  send: () => Promise<void>;

  // history
  history: Record<string, ResponseRecord[]>;
  clearHistory: (operationId: string) => void;
}

const emptyDraft: RequestDraft = {
  method: "GET",
  url: "",
  params: {},
  headers: {},
  body: "",
  bearerToken: "",
};

export const usePlayground = create<PlaygroundState>((set, get) => ({
  spec: null,
  specError: null,
  loadingSpec: false,

  loadSpec: async () => {
    set({ loadingSpec: true, specError: null });
    try {
      const res = await fetch("/openapi/openapi.json");
      if (!res.ok) {
        throw new Error(`HTTP ${res.status} fetching spec`);
      }
      const spec = (await res.json()) as OpenAPIV3.Document;
      set({ spec, loadingSpec: false });
    } catch (e) {
      set({
        specError: e instanceof Error ? e.message : String(e),
        loadingSpec: false,
      });
    }
  },

  selectedOperationId: null,
  selectEndpoint: (id) => set({ selectedOperationId: id }),

  current: { ...emptyDraft },
  setMethod: (m) => set((s) => ({ current: { ...s.current, method: m } })),
  setUrl: (u) => set((s) => ({ current: { ...s.current, url: u } })),
  setParam: (name, value) =>
    set((s) => ({
      current: { ...s.current, params: { ...s.current.params, [name]: value } },
    })),
  setHeader: (name, value) =>
    set((s) => ({
      current: { ...s.current, headers: { ...s.current.headers, [name]: value } },
    })),
  setBody: (raw) => set((s) => ({ current: { ...s.current, body: raw } })),
  setBearerToken: (t) =>
    set((s) => ({ current: { ...s.current, bearerToken: t } })),
  resetCurrent: (draft) =>
    set((s) => ({ current: { ...emptyDraft, ...draft } })),

  lastResponse: null,
  inFlight: false,
  send: async () => {
    // Implementation lands in M4.
    throw new Error("send() not yet implemented — M4");
  },

  history: {},
  clearHistory: (operationId) =>
    set((s) => {
      const { [operationId]: _, ...rest } = s.history;
      return { history: rest };
    }),
}));
```

- [ ] **Step 2: Write the spec helper**

Write to `plugins/umbral-playground/frontend/state/spec.ts`:

```typescript
import type { OpenAPIV3 } from "openapi-types";

/** An operation with its parent path and method, for the tree. */
export interface TreeEntry {
  operationId: string;
  method: string;
  path: string;
  summary?: string;
  tag: string;
}

/** Walk a spec and produce a flat list of operations, grouped by tag. */
export function listOperations(spec: OpenAPIV3.Document): TreeEntry[] {
  const out: TreeEntry[] = [];
  for (const [path, pathItem] of Object.entries(spec.paths ?? {})) {
    if (!pathItem) continue;
    const methods: Array<[string, OpenAPIV3.OperationObject | undefined]> = [
      ["GET", pathItem.get],
      ["POST", pathItem.post],
      ["PUT", pathItem.put],
      ["PATCH", pathItem.patch],
      ["DELETE", pathItem.delete],
    ];
    for (const [method, op] of methods) {
      if (!op) continue;
      const operationId = op.operationId ?? `${method} ${path}`;
      const tag = op.tags?.[0] ?? "default";
      out.push({
        operationId,
        method,
        path,
        summary: op.summary,
        tag,
      });
    }
  }
  return out;
}

/** Find an operation by id, or null. */
export function findOperation(
  spec: OpenAPIV3.Document,
  operationId: string | null,
): { method: string; path: string; op: OpenAPIV3.OperationObject } | null {
  if (!operationId) return null;
  for (const [path, pathItem] of Object.entries(spec.paths ?? {})) {
    if (!pathItem) continue;
    const methods: Array<[string, OpenAPIV3.OperationObject | undefined]> = [
      ["GET", pathItem.get],
      ["POST", pathItem.post],
      ["PUT", pathItem.put],
      ["PATCH", pathItem.patch],
      ["DELETE", pathItem.delete],
    ];
    for (const [method, op] of methods) {
      if (!op) continue {
        const id = op.operationId ?? `${method} ${path}`;
        if (id === operationId) {
          return { method, path, op };
        }
      }
    }
  }
  return null;
}
```

Wait — there's a syntax error above (`if (!op) continue { ... }` should be `if (!op) continue;`). Fix when transcribing.

- [ ] **Step 3: Write the placeholder `buildFetchArgs.ts`**

Write to `plugins/umbral-playground/frontend/state/buildFetchArgs.ts`:

```typescript
import type { RequestDraft } from "./store";

export interface FetchArgs {
  url: string;
  init: RequestInit;
}

export type BuildError =
  | { kind: "missing_path_param"; name: string }
  | { kind: "invalid_json_body"; message: string };

export function buildFetchArgs(draft: RequestDraft): {
  ok: true; args: FetchArgs;
} | { ok: false; error: BuildError } {
  // Full implementation lands in M4. This is a stub that errors so
  // we can wire up the error path in tests before the real logic.
  return {
    ok: false,
    error: { kind: "invalid_json_body", message: "not yet implemented" },
  };
}
```

- [ ] **Step 4: Write the placeholder `curl.ts`**

Write to `plugins/umbral-playground/frontend/state/curl.ts`:

```typescript
import type { RequestDraft } from "./store";

/** Render a `curl` command equivalent to the given draft. */
export function toCurl(draft: RequestDraft): string {
  const parts: string[] = [`curl -X ${draft.method}`];
  for (const [k, v] of Object.entries(draft.headers)) {
    parts.push(`-H '${k}: ${v.replace(/'/g, "'\\''")}'`);
  }
  if (draft.bearerToken) {
    parts.push(`-H 'Authorization: Bearer ${draft.bearerToken}'`);
  }
  if (draft.body && draft.method !== "GET" && draft.method !== "HEAD") {
    parts.push(`--data '${draft.body.replace(/'/g, "'\\''")}'`);
  }
  parts.push(`'${draft.url}'`);
  return parts.join(" ");
}
```

- [ ] **Step 5: Write the placeholder `history.ts`**

Write to `plugins/umbral-playground/frontend/state/history.ts`:

```typescript
import type { ResponseRecord } from "./store";

const STORAGE_KEY = "umbral-playground:history:v1";
const PER_OPERATION_CAP = 50;
const TOTAL_BYTE_CAP = 5 * 1024 * 1024; // 5MB

export function loadHistory(): Record<string, ResponseRecord[]> {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return {};
    return JSON.parse(raw) as Record<string, ResponseRecord[]>;
  } catch {
    return {};
  }
}

let saveTimer: ReturnType<typeof setTimeout> | null = null;

export function saveHistoryDebounced(
  history: Record<string, ResponseRecord[]>,
): void {
  if (saveTimer) clearTimeout(saveTimer);
  saveTimer = setTimeout(() => {
    const trimmed = enforceCaps(history);
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(trimmed));
    } catch {
      // localStorage full or disabled; silently drop. The in-memory
      // history still works for the current session.
    }
  }, 500);
}

function enforceCaps(
  history: Record<string, ResponseRecord[]>,
): Record<string, ResponseRecord[]> {
  // Per-operation cap.
  const out: Record<string, ResponseRecord[]> = {};
  for (const [k, v] of Object.entries(history)) {
    out[k] = v.slice(-PER_OPERATION_CAP);
  }
  // Total byte cap.
  let serialized = JSON.stringify(out);
  if (serialized.length <= TOTAL_BYTE_CAP) return out;

  // Drop oldest across all operations until under cap.
  const allEntries: Array<[string, ResponseRecord, number]> = [];
  for (const [opId, records] of Object.entries(out)) {
    for (let i = 0; i < records.length; i++) {
      allEntries.push([opId, records[i], records[i].timestamp]);
    }
  }
  allEntries.sort((a, b) => a[2] - b[2]);
  while (serialized.length > TOTAL_BYTE_CAP && allEntries.length > 0) {
    const [opId, record] = allEntries.shift()!;
    out[opId] = out[opId].filter((r) => r !== record);
    serialized = JSON.stringify(out);
  }
  return out;
}
```

- [ ] **Step 6: Commit**

```bash
git add plugins/umbral-playground/frontend/state/
git commit -m "feat(playground): zustand store skeleton + spec/curl/history helpers"
```

### Task 9: Components — App, Header, Tabs, MethodBadge, EmptyState, ErrorBanner

**Files:**
- Create: `plugins/umbral-playground/frontend/components/App.tsx`
- Create: `plugins/umbral-playground/frontend/components/Header.tsx`
- Create: `plugins/umbral-playground/frontend/components/Tabs.tsx`
- Create: `plugins/umbral-playground/frontend/components/MethodBadge.tsx`
- Create: `plugins/umbral-playground/frontend/components/EmptyState.tsx`
- Create: `plugins/umbral-playground/frontend/components/ErrorBanner.tsx`

- [ ] **Step 1: Write `MethodBadge.tsx`**

Write to `plugins/umbral-playground/frontend/components/MethodBadge.tsx`:

```tsx
const COLORS: Record<string, string> = {
  GET: "bg-indigo-500/20 text-indigo-300 border-indigo-500/30",
  POST: "bg-emerald-500/20 text-emerald-300 border-emerald-500/30",
  PUT: "bg-amber-500/20 text-amber-300 border-amber-500/30",
  PATCH: "bg-sky-500/20 text-sky-300 border-sky-500/30",
  DELETE: "bg-rose-500/20 text-rose-300 border-rose-500/30",
};

export function MethodBadge({ method }: { method: string }) {
  const cls = COLORS[method.toUpperCase()] ?? "bg-slate-500/20 text-slate-300 border-slate-500/30";
  return (
    <span
      className={`inline-block px-1.5 py-0.5 rounded text-[10px] font-mono font-semibold border ${cls}`}
    >
      {method.toUpperCase()}
    </span>
  );
}
```

- [ ] **Step 2: Write `Tabs.tsx`**

Write to `plugins/umbral-playground/frontend/components/Tabs.tsx`:

```tsx
import { useState, type ReactNode } from "react";

export interface Tab {
  id: string;
  label: string;
  content: ReactNode;
}

export function Tabs({ tabs, initial }: { tabs: Tab[]; initial?: string }) {
  const [active, setActive] = useState(initial ?? tabs[0]?.id);
  const current = tabs.find((t) => t.id === active) ?? tabs[0];
  return (
    <div className="flex flex-col h-full">
      <div className="flex border-b border-slate-800">
        {tabs.map((t) => (
          <button
            key={t.id}
            type="button"
            onClick={() => setActive(t.id)}
            className={`px-3 py-1.5 text-xs font-mono uppercase tracking-wider ${
              t.id === active
                ? "text-slate-200 border-b-2 border-indigo-400"
                : "text-slate-500 hover:text-slate-300"
            }`}
          >
            {t.label}
          </button>
        ))}
      </div>
      <div className="flex-1 overflow-auto p-3">{current?.content}</div>
    </div>
  );
}
```

- [ ] **Step 3: Write `EmptyState.tsx`**

Write to `plugins/umbral-playground/frontend/components/EmptyState.tsx`:

```tsx
import type { ReactNode } from "react";

export function EmptyState({
  title,
  children,
}: {
  title: string;
  children?: ReactNode;
}) {
  return (
    <div className="flex items-center justify-center h-full text-slate-500 text-sm">
      <div className="text-center">
        <p className="font-mono text-xs uppercase tracking-widest mb-2">{title}</p>
        {children}
      </div>
    </div>
  );
}
```

- [ ] **Step 4: Write `ErrorBanner.tsx`**

Write to `plugins/umbral-playground/frontend/components/ErrorBanner.tsx`:

```tsx
export function ErrorBanner({
  message,
  onRetry,
}: {
  message: string;
  onRetry?: () => void;
}) {
  return (
    <div className="border-b border-rose-900/50 bg-rose-950/30 px-3 py-2 flex items-center justify-between gap-3">
      <span className="text-xs font-mono text-rose-300">
        <span className="font-semibold mr-2">Error:</span>
        {message}
      </span>
      {onRetry && (
        <button
          type="button"
          onClick={onRetry}
          className="text-[10px] font-mono uppercase tracking-widest text-rose-200 hover:text-white px-2 py-0.5 rounded focus-visible:outline focus-visible:outline-2 focus-visible:outline-rose-400"
        >
          Retry
        </button>
      )}
    </div>
  );
}
```

- [ ] **Step 5: Write `Header.tsx`**

Write to `plugins/umbral-playground/frontend/components/Header.tsx`:

```tsx
import { usePlayground } from "../state/store";

export function Header() {
  const spec = usePlayground((s) => s.spec);
  const loadSpec = usePlayground((s) => s.loadSpec);

  return (
    <header className="border-b border-slate-800 px-4 py-2 flex items-center justify-between bg-slate-950/60">
      <div className="flex items-baseline gap-3">
        <span className="font-mono text-xs tracking-widest text-slate-500">umbral</span>
        <span className="font-mono text-xs text-slate-300">
          {spec?.info?.title ?? "playground"}
        </span>
        {spec?.info?.version && (
          <span className="font-mono text-[10px] text-slate-500">
            v{spec.info.version}
          </span>
        )}
      </div>
      <button
        type="button"
        onClick={() => void loadSpec()}
        className="text-[10px] font-mono uppercase tracking-widest text-slate-500 hover:text-slate-200 px-2 py-1 rounded focus-visible:outline focus-visible:outline-2 focus-visible:outline-indigo-400"
      >
        Reload spec
      </button>
    </header>
  );
}
```

- [ ] **Step 6: Write `App.tsx`**

Write to `plugins/umbral-playground/frontend/components/App.tsx`:

```tsx
import { useEffect } from "react";
import { usePlayground } from "../state/store";
import { Header } from "./Header";
import { EndpointTree } from "./EndpointTree";
import { RequestBuilder } from "./RequestBuilder";
import { ResponseViewer } from "./ResponseViewer";
import { ErrorBanner } from "./ErrorBanner";
import { EmptyState } from "./EmptyState";

export function App() {
  const loadSpec = usePlayground((s) => s.loadSpec);
  const specError = usePlayground((s) => s.specError);
  const spec = usePlayground((s) => s.spec);
  const loadingSpec = usePlayground((s) => s.loadingSpec);

  useEffect(() => {
    void loadSpec();
  }, [loadSpec]);

  return (
    <div className="h-screen flex flex-col bg-slate-950 text-slate-300">
      <Header />
      {specError && <ErrorBanner message={specError} onRetry={() => void loadSpec()} />}
      <div className="flex-1 grid grid-cols-[240px_1fr_1fr] overflow-hidden">
        <aside className="border-r border-slate-800 overflow-y-auto">
          <EndpointTree />
        </aside>
        <main className="border-r border-slate-800 overflow-hidden flex flex-col">
          <RequestBuilder />
        </main>
        <section className="overflow-hidden flex flex-col">
          <ResponseViewer />
        </section>
      </div>
    </div>
  );
}
```

- [ ] **Step 7: Commit**

```bash
git add plugins/umbral-playground/frontend/components/
git commit -m "feat(playground): app shell, header, tabs, badge, empty/error states"
```

### Task 10: Components — EndpointTree, RequestBuilder (stub), ResponseViewer (stub)

**Files:**
- Create: `plugins/umbral-playground/frontend/components/EndpointTree.tsx`
- Create: `plugins/umbral-playground/frontend/components/RequestBuilder.tsx`
- Create: `plugins/umbral-playground/frontend/components/ResponseViewer.tsx`
- Create: `plugins/umbral-playground/frontend/index.tsx`
- Create: `plugins/umbral-playground/frontend/index.html`
- Create: `plugins/umbral-playground/frontend/styles/tailwind.config.js`
- Create: `plugins/umbral-playground/frontend/styles/app.css`

- [ ] **Step 1: Write `EndpointTree.tsx`**

Write to `plugins/umbral-playground/frontend/components/EndpointTree.tsx`:

```tsx
import { useMemo, useState } from "react";
import { usePlayground } from "../state/store";
import { listOperations, type TreeEntry } from "../state/spec";
import { MethodBadge } from "./MethodBadge";
import { EmptyState } from "./EmptyState";

export function EndpointTree() {
  const spec = usePlayground((s) => s.spec);
  const loadingSpec = usePlayground((s) => s.loadingSpec);
  const selected = usePlayground((s) => s.selectedOperationId);
  const select = usePlayground((s) => s.selectEndpoint);
  const [search, setSearch] = useState("");

  const grouped = useMemo(() => {
    if (!spec) return null;
    const all = listOperations(spec);
    const q = search.toLowerCase();
    const filtered = q
      ? all.filter(
          (e) =>
            e.path.toLowerCase().includes(q) ||
            e.method.toLowerCase().includes(q) ||
            (e.summary?.toLowerCase().includes(q) ?? false),
        )
      : all;
    const byTag = new Map<string, TreeEntry[]>();
    for (const e of filtered) {
      const list = byTag.get(e.tag) ?? [];
      list.push(e);
      byTag.set(e.tag, list);
    }
    return Array.from(byTag.entries()).sort(([a], [b]) => a.localeCompare(b));
  }, [spec, search]);

  if (loadingSpec) {
    return <EmptyState title="Loading spec..." />;
  }
  if (!spec) {
    return <EmptyState title="No spec loaded" />;
  }
  if (!grouped || grouped.length === 0) {
    return (
      <div className="p-3">
        <input
          className="w-full px-2 py-1 bg-slate-900 border border-slate-800 rounded text-xs font-mono text-slate-200 placeholder-slate-600"
          placeholder="Search endpoints..."
          value={search}
          onChange={(e) => setSearch(e.target.value)}
        />
        <EmptyState title="No matches" />
      </div>
    );
  }

  return (
    <div className="p-2 space-y-1">
      <input
        className="w-full px-2 py-1 bg-slate-900 border border-slate-800 rounded text-xs font-mono text-slate-200 placeholder-slate-600 focus:outline-none focus:border-indigo-500"
        placeholder="Search endpoints..."
        value={search}
        onChange={(e) => setSearch(e.target.value)}
      />
      {grouped.map(([tag, entries]) => (
        <details key={tag} open className="group">
          <summary className="cursor-pointer px-2 py-1 text-[10px] font-mono uppercase tracking-widest text-slate-500 hover:text-slate-300 select-none">
            {tag} <span className="text-slate-700">({entries.length})</span>
          </summary>
          <ul className="mt-1 space-y-0.5">
            {entries.map((e) => (
              <li key={e.operationId}>
                <button
                  type="button"
                  onClick={() => select(e.operationId)}
                  className={`w-full text-left px-2 py-1 rounded text-xs flex items-center gap-2 ${
                    selected === e.operationId
                      ? "bg-indigo-500/20 text-slate-100"
                      : "hover:bg-slate-900 text-slate-400"
                  }`}
                >
                  <MethodBadge method={e.method} />
                  <span className="font-mono truncate">{e.path}</span>
                </button>
              </li>
            ))}
          </ul>
        </details>
      ))}
    </div>
  );
}
```

- [ ] **Step 2: Write a stub `RequestBuilder.tsx` for M3**

Write to `plugins/umbral-playground/frontend/components/RequestBuilder.tsx`:

```tsx
import { useEffect } from "react";
import { usePlayground } from "../state/store";
import { findOperation } from "../state/spec";
import { EmptyState } from "./EmptyState";

/**
 * M3 stub: shows the URL strip populated when an endpoint is selected.
 * Full implementation (tabs, body editor, etc.) lands in M4.
 */
export function RequestBuilder() {
  const spec = usePlayground((s) => s.spec);
  const selected = usePlayground((s) => s.selectedOperationId);
  const current = usePlayground((s) => s.current);
  const resetCurrent = usePlayground((s) => s.resetCurrent);

  const op = spec && selected ? findOperation(spec, selected) : null;

  useEffect(() => {
    if (op) {
      resetCurrent({ method: op.method, url: op.path });
    }
  }, [op?.method, op?.path, resetCurrent]);

  if (!selected) {
    return <EmptyState title="Select an endpoint" />;
  }
  return (
    <div className="p-3 space-y-2">
      <div className="flex items-center gap-2">
        <span className="font-mono text-xs px-2 py-1 rounded bg-slate-900 border border-slate-800 text-slate-300">
          {current.method}
        </span>
        <code className="font-mono text-sm text-slate-200 flex-1 truncate">
          {current.url}
        </code>
      </div>
      <p className="font-mono text-[10px] text-slate-600">
        Request builder tabs land in M4.
      </p>
    </div>
  );
}
```

- [ ] **Step 3: Write a stub `ResponseViewer.tsx` for M3**

Write to `plugins/umbral-playground/frontend/components/ResponseViewer.tsx`:

```tsx
import { EmptyState } from "./EmptyState";

export function ResponseViewer() {
  return <EmptyState title="Send a request to see the response" />;
}
```

- [ ] **Step 4: Write `index.tsx`**

Write to `plugins/umbral-playground/frontend/index.tsx`:

```tsx
import { createRoot } from "react-dom/client";
import { App } from "./components/App";

const container = document.getElementById("root");
if (!container) {
  throw new Error("umbral-playground: #root not found");
}
createRoot(container).render(<App />);
```

- [ ] **Step 5: Write `index.html`**

Write to `plugins/umbral-playground/frontend/index.html`:

```html
<!doctype html>
<html lang="en">
<head>
  <meta charset="UTF-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1.0" />
  <title>umbral playground</title>
</head>
<body>
  <div id="root"></div>
</body>
</html>
```

This is the *source* HTML; the served `shell.html` is the runtime variant. `esbuild` doesn't use `index.html` directly; the entry is `index.tsx`. We keep `index.html` for `npm run dev` and any future SSR-adjacent tooling.

- [ ] **Step 6: Write `tailwind.config.js`**

Write to `plugins/umbral-playground/frontend/styles/tailwind.config.js`:

```javascript
/** @type {import('tailwindcss').Config} */
export default {
  content: ["./index.html", "./**/*.{ts,tsx}"],
  theme: {
    extend: {},
  },
  plugins: [],
};
```

- [ ] **Step 7: Write `app.css`**

Write to `plugins/umbral-playground/frontend/styles/app.css`:

```css
@tailwind base;
@tailwind components;
@tailwind utilities;
```

- [ ] **Step 8: Commit**

```bash
git add plugins/umbral-playground/frontend/
git commit -m "feat(playground): m3 frontend — tree, URL strip, full app shell"
```

### Task 11: M3 verification — visual smoke test

This milestone produces a visible UI. Verification is "build the plugin with the CLIs, serve it, and look at the page in a browser." Without the CLIs, the placeholder page is the right outcome.

**Files:** none.

- [ ] **Step 1: Check whether esbuild + tailwindcss are available**

Run: `which esbuild tailwindcss 2>&1`

If both present: continue.
If not: skip to Step 4. The placeholder page is the correct degraded outcome.

- [ ] **Step 2: Install frontend deps (only if running the happy path)**

Run: `cd plugins/umbral-playground/frontend && npm install --no-audit --no-fund 2>&1 | tail -5`

Expected: `added N packages` with no errors.

- [ ] **Step 3: Build the plugin**

Run: `cd ../../../crates && cargo clean -p umbral-playground && cargo build -p umbral-playground 2>&1 | tail -10`

Expected: no `cargo:warning=...` about missing CLIs. `Finished ...` for the crate. The `dist/` directory should now contain `playground.<hash>.{js,css}`.

- [ ] **Step 4: Document the manual visual check in the README**

In `plugins/umbral-playground/README.md`, add a "Manual smoke test" section:

```markdown
## Manual smoke test

After `cargo build -p umbral-playground` with the CLIs installed:

1. Run an example app that registers `RestPlugin`, `OpenApiPlugin`, and `PlaygroundPlugin`.
2. Open `http://localhost:<port>/api/playground/` in a browser.
3. You should see the 3-pane shell with a left endpoint tree, a center URL strip, and an empty right pane.
4. The left pane should show a "Loading spec..." state for a moment, then a list of endpoints grouped by tag.
5. Click an endpoint; the center URL strip should populate with the method and path.
```

The README file is `Create`, not `Modify`. Write it now:

Write to `plugins/umbral-playground/README.md`:

```markdown
# umbral-playground

Interactive API playground UI for umbral-rest. A 3-pane Postman-style UI mounted at `/api/playground/`. Fetches the existing `umbral-openapi` JSON spec at runtime and renders a navigable endpoint tree, request builder, and response viewer.

## Quick start

```rust
use umbral_playground::PlaygroundPlugin;
use umbral_rest::RestPlugin;
use umbral_openapi::OpenApiPlugin;

let app = App::builder()
    .plugin(RestPlugin::default())
    .plugin(OpenApiPlugin::new())
    .plugin(PlaygroundPlugin::new())    // mounts at /api/playground/
    .build();
```

`cargo build` produces the bundled React UI. Requires `esbuild` and `tailwindcss` in `$PATH` (Node 20+). Install with:

```
npm i -g esbuild tailwindcss
```

If either is missing, the plugin still compiles and serves a placeholder page that explains what to install.

## Configuration

```rust
PlaygroundPlugin::new().at("/api/docs/playground")  // mount elsewhere
```

## v1 limitations

- Same-origin only (no CORS proxy)
- Auth is a single bearer-token input
- Body is a JSON textarea (no schema-driven form)
- Request history is localStorage-only (per browser, per device)
- Pane sizes are fixed

See `docs/superpowers/specs/2026-06-02-rest-playground-design.md` for the full design.

## Manual smoke test

After `cargo build -p umbral-playground` with the CLIs installed:

1. Run an example app that registers `RestPlugin`, `OpenApiPlugin`, and `PlaygroundPlugin`.
2. Open `http://localhost:<port>/api/playground/` in a browser.
3. You should see the 3-pane shell with a left endpoint tree, a center URL strip, and an empty right pane.
4. The left pane should show a "Loading spec..." state for a moment, then a list of endpoints grouped by tag.
5. Click an endpoint; the center URL strip should populate with the method and path.
```

- [ ] **Step 5: Commit**

```bash
git add plugins/umbral-playground/README.md
git commit -m "docs(playground): README with quickstart and v1 limitations"
```

---

## Milestone 4: Request execution + full request/response UI

Goal: the playground actually sends requests and renders the response. All four request tabs (Params, Body, Headers, Auth) and all four response tabs (Body, Headers, cURL, History stub) work. The `send()` action wires up to a real `fetch()`.

### Task 12: Implement `buildFetchArgs` properly

**Files:**
- Modify: `plugins/umbral-playground/frontend/state/buildFetchArgs.ts`

- [ ] **Step 1: Write the failing test**

Write to `plugins/umbral-playground/frontend/__tests__/buildFetchArgs.test.ts`:

```typescript
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
```

- [ ] **Step 2: Install deps so Vitest can run**

Run: `cd plugins/umbral-playground/frontend && npm install --no-audit --no-fund 2>&1 | tail -5`

Expected: `added N packages`.

- [ ] **Step 3: Run the test to verify it fails**

Run: `cd plugins/umbral-playground/frontend && npx vitest run __tests__/buildFetchArgs.test.ts 2>&1 | tail -30`

Expected: most tests fail with `ok: false, error: "not yet implemented"`. The test runner reports the failures.

- [ ] **Step 4: Implement `buildFetchArgs`**

Replace `plugins/umbral-playground/frontend/state/buildFetchArgs.ts` with:

```typescript
import type { RequestDraft } from "./store";

export interface FetchArgs {
  url: string;
  init: RequestInit;
}

export type BuildError =
  | { kind: "missing_path_param"; name: string }
  | { kind: "invalid_json_body"; message: string };

export function buildFetchArgs(draft: RequestDraft): {
  ok: true; args: FetchArgs;
} | { ok: false; error: BuildError } {
  // 1. Resolve path template params.
  let url = draft.url;
  const templateNames = [...url.matchAll(/\{([a-zA-Z_][a-zA-Z0-9_]*)\}/g)].map((m) => m[1]);
  for (const name of templateNames) {
    const value = draft.params[name];
    if (!value) {
      return { ok: false, error: { kind: "missing_path_param", name } };
    }
    url = url.replace(`{${name}}`, encodeURIComponent(value));
  }

  // 2. Build query string.
  const queryEntries = Object.entries(draft.params).filter(
    ([k]) => !templateNames.includes(k),
  );
  if (queryEntries.length > 0) {
    const qs = queryEntries
      .map(
        ([k, v]) => `${encodeURIComponent(k)}=${encodeURIComponent(v)}`,
      )
      .join("&");
    url += url.includes("?") ? `&${qs}` : `?${qs}`;
  }

  // 3. Headers.
  const headers: Record<string, string> = { ...draft.headers };
  if (draft.bearerToken) {
    headers["Authorization"] = `Bearer ${draft.bearerToken}`;
  }

  // 4. Body.
  const method = draft.method.toUpperCase();
  let body: string | undefined;
  if (draft.body && method !== "GET" && method !== "HEAD") {
    if (
      !headers["Content-Type"] ||
      headers["Content-Type"].includes("application/json")
    ) {
      try {
        JSON.parse(draft.body);
      } catch (e) {
        return {
          ok: false,
          error: {
            kind: "invalid_json_body",
            message: e instanceof Error ? e.message : String(e),
          },
        };
      }
      if (!headers["Content-Type"]) {
        headers["Content-Type"] = "application/json";
      }
    }
    body = draft.body;
  }

  return {
    ok: true,
    args: {
      url,
      init: { method, headers, body },
    },
  };
}
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cd plugins/umbral-playground/frontend && npx vitest run __tests__/buildFetchArgs.test.ts 2>&1 | tail -15`

Expected: all 10 tests pass.

- [ ] **Step 6: Commit**

```bash
git add plugins/umbral-playground/frontend/state/buildFetchArgs.ts plugins/umbral-playground/frontend/__tests__/buildFetchArgs.test.ts plugins/umbral-playground/frontend/package-lock.json plugins/umbral-playground/frontend/node_modules/
git commit -m "feat(playground): buildFetchArgs with full template + body + header logic"
```

Wait — `node_modules/` is gitignored. Don't add it. The `package-lock.json` *should* be committed (so the lockfile pins the dependency tree). Update the commit:

```bash
git add plugins/umbral-playground/frontend/state/buildFetchArgs.ts plugins/umbral-playground/frontend/__tests__/buildFetchArgs.test.ts plugins/umbral-playground/frontend/package-lock.json
git commit -m "feat(playground): buildFetchArgs with full template + body + header logic"
```

### Task 13: Wire up `send()` in the store

**Files:**
- Modify: `plugins/umbral-playground/frontend/state/store.ts`

- [ ] **Step 1: Update the `send` implementation**

In `plugins/umbral-playground/frontend/state/store.ts`, replace the `send` action:

```typescript
  send: async () => {
    const state = get();
    const result = buildFetchArgs(state.current);
    if (!result.ok) {
      const message =
        result.error.kind === "missing_path_param"
          ? `Missing path parameter: ${result.error.name}`
          : `Invalid JSON body: ${result.error.message}`;
      set({
        lastResponse: {
          operationId: state.selectedOperationId ?? "unknown",
          request: { ...state.current },
          status: 0,
          statusText: "Build error",
          durationMs: 0,
          sizeBytes: 0,
          headers: {},
          bodyText: message,
          timestamp: Date.now(),
          error: message,
        },
      });
      return;
    }

    set({ inFlight: true });
    const start = performance.now();
    try {
      const res = await fetch(result.args.url, result.args.init);
      const bodyText = await res.text();
      const durationMs = performance.now() - start;
      const headers: Record<string, string> = {};
      res.headers.forEach((v, k) => {
        headers[k] = v;
      });
      const record: ResponseRecord = {
        operationId: state.selectedOperationId ?? "unknown",
        request: { ...state.current },
        status: res.status,
        statusText: res.statusText,
        durationMs: Math.round(durationMs),
        sizeBytes: new Blob([bodyText]).size,
        headers,
        bodyText,
        timestamp: Date.now(),
      };
      set((s) => {
        const opId = state.selectedOperationId ?? "unknown";
        const existing = s.history[opId] ?? [];
        return {
          lastResponse: record,
          inFlight: false,
          history: {
            ...s.history,
            [opId]: [...existing, record],
          },
        };
      });
      saveHistoryDebounced(get().history);
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      set({
        inFlight: false,
        lastResponse: {
          operationId: state.selectedOperationId ?? "unknown",
          request: { ...state.current },
          status: 0,
          statusText: "Network error",
          durationMs: Math.round(performance.now() - start),
          sizeBytes: 0,
          headers: {},
          bodyText: "",
          timestamp: Date.now(),
          error: message,
        },
      });
    }
  },
```

- [ ] **Step 2: Add the imports at the top of the file**

In `plugins/umbral-playground/frontend/state/store.ts`, the new `send` references `buildFetchArgs` and `saveHistoryDebounced`. Add at the top:

```typescript
import { buildFetchArgs } from "./buildFetchArgs";
import { saveHistoryDebounced } from "./history";
```

And update the `create` import:

```typescript
import { create } from "zustand";
import type { OpenAPIV3 } from "openapi-types";
```

is already there. Good.

- [ ] **Step 3: Verify TypeScript compiles**

Run: `cd plugins/umbral-playground/frontend && npx tsc --noEmit 2>&1 | tail -20`

Expected: no errors. If you see errors about missing types, run `npm install` again — some types are pulled in transitively.

- [ ] **Step 4: Commit**

```bash
git add plugins/umbral-playground/frontend/state/store.ts
git commit -m "feat(playground): send() action wires up fetch + history append"
```

### Task 14: Replace the request builder stub with the full UI

**Files:**
- Modify: `plugins/umbral-playground/frontend/components/RequestBuilder.tsx`
- Create: `plugins/umbral-playground/frontend/components/KeyValueTable.tsx`
- Create: `plugins/umbral-playground/frontend/components/AuthTab.tsx`

- [ ] **Step 1: Write `KeyValueTable.tsx`**

Write to `plugins/umbral-playground/frontend/components/KeyValueTable.tsx`:

```tsx
import { useState } from "react";

export interface KV {
  key: string;
  value: string;
}

export function KeyValueTable({
  rows,
  onChange,
  keyPlaceholder = "key",
  valuePlaceholder = "value",
}: {
  rows: KV[];
  onChange: (rows: KV[]) => void;
  keyPlaceholder?: string;
  valuePlaceholder?: string;
}) {
  const [adding, setAdding] = useState(false);
  const [draft, setDraft] = useState<KV>({ key: "", value: "" });

  const update = (i: number, patch: Partial<KV>) => {
    const next = rows.map((r, idx) => (idx === i ? { ...r, ...patch } : r));
    onChange(next);
  };
  const remove = (i: number) => {
    onChange(rows.filter((_, idx) => idx !== i));
  };
  const commit = () => {
    if (!draft.key && !draft.value) {
      setAdding(false);
      return;
    }
    onChange([...rows, draft]);
    setDraft({ key: "", value: "" });
    setAdding(false);
  };

  return (
    <div className="space-y-1.5 text-xs">
      {rows.map((row, i) => (
        <div key={i} className="flex gap-1.5">
          <input
            value={row.key}
            onChange={(e) => update(i, { key: e.target.value })}
            placeholder={keyPlaceholder}
            className="flex-1 px-2 py-1 bg-slate-900 border border-slate-800 rounded font-mono text-slate-200 placeholder-slate-600 focus:outline-none focus:border-indigo-500"
          />
          <input
            value={row.value}
            onChange={(e) => update(i, { value: e.target.value })}
            placeholder={valuePlaceholder}
            className="flex-[2] px-2 py-1 bg-slate-900 border border-slate-800 rounded font-mono text-slate-200 placeholder-slate-600 focus:outline-none focus:border-indigo-500"
          />
          <button
            type="button"
            onClick={() => remove(i)}
            className="px-2 py-1 text-slate-500 hover:text-rose-400"
            aria-label="Remove"
          >
            ×
          </button>
        </div>
      ))}
      {adding ? (
        <div className="flex gap-1.5">
          <input
            autoFocus
            value={draft.key}
            onChange={(e) => setDraft({ ...draft, key: e.target.value })}
            placeholder={keyPlaceholder}
            className="flex-1 px-2 py-1 bg-slate-900 border border-slate-800 rounded font-mono text-slate-200 placeholder-slate-600 focus:outline-none focus:border-indigo-500"
          />
          <input
            value={draft.value}
            onChange={(e) => setDraft({ ...draft, value: e.target.value })}
            placeholder={valuePlaceholder}
            onKeyDown={(e) => {
              if (e.key === "Enter") commit();
              if (e.key === "Escape") {
                setAdding(false);
                setDraft({ key: "", value: "" });
              }
            }}
            className="flex-[2] px-2 py-1 bg-slate-900 border border-slate-800 rounded font-mono text-slate-200 placeholder-slate-600 focus:outline-none focus:border-indigo-500"
          />
          <button
            type="button"
            onClick={commit}
            className="px-2 py-1 text-indigo-300 hover:text-indigo-200"
          >
            ✓
          </button>
        </div>
      ) : (
        <button
          type="button"
          onClick={() => setAdding(true)}
          className="text-[10px] font-mono uppercase tracking-widest text-slate-500 hover:text-slate-200"
        >
          + add row
        </button>
      )}
    </div>
  );
}
```

- [ ] **Step 2: Write `AuthTab.tsx`**

Write to `plugins/umbral-playground/frontend/components/AuthTab.tsx`:

```tsx
import { usePlayground } from "../state/store";

export function AuthTab() {
  const bearer = usePlayground((s) => s.current.bearerToken);
  const setBearer = usePlayground((s) => s.setBearerToken);

  return (
    <div className="space-y-3 text-xs">
      <div>
        <label className="block font-mono text-[10px] uppercase tracking-widest text-slate-500 mb-1">
          Bearer token
        </label>
        <input
          type="password"
          value={bearer}
          onChange={(e) => setBearer(e.target.value)}
          placeholder="paste token here"
          className="w-full px-2 py-1 bg-slate-900 border border-slate-800 rounded font-mono text-slate-200 placeholder-slate-600 focus:outline-none focus:border-indigo-500"
        />
        <p className="mt-1.5 text-[10px] text-slate-600">
          Sent as <code className="font-mono text-slate-400">Authorization: Bearer ...</code> on every request.
        </p>
      </div>
      <p className="text-[10px] text-slate-600 leading-relaxed">
        For session-based auth, log into the app in another tab first. The
        playground shares cookies with the rest of the app.
      </p>
    </div>
  );
}
```

- [ ] **Step 3: Replace `RequestBuilder.tsx` with the full UI**

Write to `plugins/umbral-playground/frontend/components/RequestBuilder.tsx`:

```tsx
import { useEffect, useMemo } from "react";
import { usePlayground } from "../state/store";
import { findOperation } from "../state/spec";
import { Tabs } from "./Tabs";
import { KeyValueTable, type KV } from "./KeyValueTable";
import { AuthTab } from "./AuthTab";
import { EmptyState } from "./EmptyState";

function kvToRecord(rows: KV[]): Record<string, string> {
  return Object.fromEntries(rows.map((r) => [r.key, r.value]).filter(([k]) => k));
}

function recordToKv(r: Record<string, string>): KV[] {
  return Object.entries(r).map(([key, value]) => ({ key, value }));
}

function buildPathParamInputs(
  path: string,
  params: Record<string, string>,
  setParam: (name: string, value: string) => void,
): JSX.Element | null {
  const names = [...path.matchAll(/\{([a-zA-Z_][a-zA-Z0-9_]*)\}/g)].map((m) => m[1]);
  if (names.length === 0) return null;
  return (
    <div className="flex flex-wrap gap-2">
      {names.map((name) => (
        <label key={name} className="flex items-center gap-1.5 text-xs">
          <span className="font-mono text-[10px] uppercase tracking-widest text-slate-500">
            {name}
          </span>
          <input
            value={params[name] ?? ""}
            onChange={(e) => setParam(name, e.target.value)}
            className="px-2 py-1 bg-slate-900 border border-slate-800 rounded font-mono text-slate-200 placeholder-slate-600 focus:outline-none focus:border-indigo-500 w-32"
            placeholder={`{${name}}`}
          />
        </label>
      ))}
    </div>
  );
}

export function RequestBuilder() {
  const spec = usePlayground((s) => s.spec);
  const selected = usePlayground((s) => s.selectedOperationId);
  const current = usePlayground((s) => s.current);
  const setUrl = usePlayground((s) => s.setUrl);
  const setParam = usePlayground((s) => s.setParam);
  const setHeader = usePlayground((s) => s.setHeader);
  const setBody = usePlayground((s) => s.setBody);
  const resetCurrent = usePlayground((s) => s.resetCurrent);
  const send = usePlayground((s) => s.send);
  const inFlight = usePlayground((s) => s.inFlight);

  const op = useMemo(
    () => (spec && selected ? findOperation(spec, selected) : null),
    [spec, selected],
  );

  useEffect(() => {
    if (op) {
      resetCurrent({ method: op.method, url: op.path });
    }
  }, [op?.method, op?.path, resetCurrent]);

  if (!selected) {
    return <EmptyState title="Select an endpoint" />;
  }

  const pathInputs = buildPathParamInputs(current.url, current.params, setParam);
  const paramRows: KV[] = recordToKv(current.params);
  const headerRows: KV[] = recordToKv(current.headers);

  return (
    <div className="flex flex-col h-full">
      <div className="p-3 space-y-2 border-b border-slate-800">
        <div className="flex items-center gap-2">
          <span className="font-mono text-xs px-2 py-1 rounded bg-slate-900 border border-slate-800 text-slate-300">
            {current.method}
          </span>
          <input
            value={current.url}
            onChange={(e) => setUrl(e.target.value)}
            className="flex-1 px-2 py-1 bg-slate-900 border border-slate-800 rounded font-mono text-sm text-slate-200 focus:outline-none focus:border-indigo-500"
          />
          <button
            type="button"
            onClick={() => void send()}
            disabled={inFlight}
            className="px-3 py-1 rounded bg-indigo-500 hover:bg-indigo-400 disabled:opacity-50 text-white text-xs font-semibold"
          >
            {inFlight ? "Sending..." : "Send"}
          </button>
        </div>
        {pathInputs}
      </div>
      <Tabs
        tabs={[
          {
            id: "params",
            label: "Params",
            content: (
              <KeyValueTable
                rows={paramRows}
                onChange={(rows) => {
                  // Replace wholesale — params are a flat map keyed by name.
                  const next: Record<string, string> = {};
                  for (const r of rows) {
                    if (r.key) next[r.key] = r.value;
                  }
                  // Diff against current to keep the store's setParam happy.
                  for (const k of Object.keys(current.params)) {
                    if (!(k in next)) {
                      setParam(k, "");
                    }
                  }
                  for (const [k, v] of Object.entries(next)) {
                    if (current.params[k] !== v) setParam(k, v);
                  }
                }}
              />
            ),
          },
          {
            id: "body",
            label: "Body",
            content: (
              <div className="space-y-2 h-full flex flex-col">
                <textarea
                  value={current.body}
                  onChange={(e) => setBody(e.target.value)}
                  className="flex-1 w-full px-2 py-1 bg-slate-950 border border-slate-800 rounded font-mono text-xs text-slate-200 placeholder-slate-600 focus:outline-none focus:border-indigo-500 resize-none"
                  placeholder="JSON body"
                />
                <button
                  type="button"
                  onClick={() => {
                    try {
                      setBody(JSON.stringify(JSON.parse(current.body), null, 2));
                    } catch {
                      /* leave as-is */
                    }
                  }}
                  className="self-start text-[10px] font-mono uppercase tracking-widest text-slate-500 hover:text-slate-200"
                >
                  Format
                </button>
              </div>
            ),
          },
          {
            id: "headers",
            label: "Headers",
            content: (
              <KeyValueTable
                rows={headerRows}
                onChange={(rows) => {
                  const next = kvToRecord(rows);
                  for (const k of Object.keys(current.headers)) {
                    if (!(k in next)) setHeader(k, "");
                  }
                  for (const [k, v] of Object.entries(next)) {
                    if (current.headers[k] !== v) setHeader(k, v);
                  }
                }}
              />
            ),
          },
          { id: "auth", label: "Auth", content: <AuthTab /> },
        ]}
      />
    </div>
  );
}
```

- [ ] **Step 4: Verify TypeScript compiles**

Run: `cd plugins/umbral-playground/frontend && npx tsc --noEmit 2>&1 | tail -20`

Expected: no errors. If `JSX.Element` is missing, add the React types import; if you see `kvToRecord` unused warnings, that's fine (it's used in Headers; lint config might still flag it — fix if so).

- [ ] **Step 5: Commit**

```bash
git add plugins/umbral-playground/frontend/components/
git commit -m "feat(playground): full request builder with all four tabs"
```

### Task 15: Replace the response viewer stub with the full UI

**Files:**
- Modify: `plugins/umbral-playground/frontend/components/ResponseViewer.tsx`
- Create: `plugins/umbral-playground/frontend/components/JsonView.tsx`

- [ ] **Step 1: Write `JsonView.tsx`**

Write to `plugins/umbral-playground/frontend/components/JsonView.tsx`:

```tsx
import { useState } from "react";

function isObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function Node({ name, value, depth }: { name: string; depth: number }) {
  const [open, setOpen] = useState(depth < 2);
  const isExpandable = isObject(value) || Array.isArray(value);
  if (!isExpandable) {
    return (
      <div className="flex gap-1.5 font-mono text-xs">
        <span className="text-slate-500">{name}:</span>
        <span className="text-emerald-300 break-all">
          {JSON.stringify(value)}
        </span>
      </div>
    );
  }
  const entries = Array.isArray(value)
    ? value.map((v, i) => [String(i), v] as const)
    : Object.entries(value);
  return (
    <div className="font-mono text-xs">
      <button
        type="button"
        onClick={() => setOpen(!open)}
        className="text-slate-400 hover:text-slate-200"
      >
        {open ? "▾" : "▸"} {name} {Array.isArray(value) ? `(${value.length})` : `{${entries.length}}`}
      </button>
      {open && (
        <div className="ml-4 border-l border-slate-800 pl-2 mt-0.5 space-y-0.5">
          {entries.map(([k, v]) => (
            <Node key={k} name={k} value={v} depth={depth + 1} />
          ))}
        </div>
      )}
    </div>
  );
}

export function JsonView({ text }: { text: string }) {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    return (
      <pre className="font-mono text-xs text-slate-300 whitespace-pre-wrap break-all">
        {text}
      </pre>
    );
  }
  if (Array.isArray(parsed)) {
    return (
      <div className="space-y-0.5">
        <Node name="[root]" value={parsed} depth={0} />
      </div>
    );
  }
  if (isObject(parsed)) {
    return (
      <div className="space-y-0.5">
        {Object.entries(parsed).map(([k, v]) => (
          <Node key={k} name={k} value={v} depth={0} />
        ))}
      </div>
    );
  }
  return (
    <pre className="font-mono text-xs text-slate-300 whitespace-pre-wrap break-all">
      {JSON.stringify(parsed, null, 2)}
    </pre>
  );
}
```

- [ ] **Step 2: Replace `ResponseViewer.tsx`**

Write to `plugins/umbral-playground/frontend/components/ResponseViewer.tsx`:

```tsx
import { usePlayground, type ResponseRecord } from "../state/store";
import { Tabs } from "./Tabs";
import { JsonView } from "./JsonView";
import { EmptyState } from "./EmptyState";
import { toCurl } from "../state/curl";

function StatusBadge({ status }: { status: number }) {
  const cls =
    status === 0
      ? "bg-slate-700 text-slate-200"
      : status < 300
      ? "bg-emerald-500/20 text-emerald-300"
      : status < 400
      ? "bg-amber-500/20 text-amber-300"
      : "bg-rose-500/20 text-rose-300";
  return (
    <span
      className={`inline-block px-2 py-0.5 rounded text-[10px] font-mono font-semibold ${cls}`}
    >
      {status === 0 ? "ERR" : status}
    </span>
  );
}

function HistoryRow({
  record,
  onRestore,
}: {
  record: ResponseRecord;
  onRestore: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onRestore}
      className="w-full text-left px-2 py-1.5 rounded hover:bg-slate-900 flex items-center gap-2 text-xs"
    >
      <StatusBadge status={record.status} />
      <span className="font-mono text-[10px] text-slate-500">
        {new Date(record.timestamp).toLocaleTimeString()}
      </span>
      <span className="font-mono text-[10px] text-slate-600">
        {record.durationMs}ms
      </span>
      <span className="font-mono text-xs text-slate-300 truncate flex-1">
        {record.request.method} {record.request.url}
      </span>
    </button>
  );
}

export function ResponseViewer() {
  const last = usePlayground((s) => s.lastResponse);
  const selected = usePlayground((s) => s.selectedOperationId);
  const history = usePlayground((s) => s.history);
  const setMethod = usePlayground((s) => s.setMethod);
  const setUrl = usePlayground((s) => s.setUrl);
  const setParam = usePlayground((s) => s.setParam);
  const setHeader = usePlayground((s) => s.setHeader);
  const setBody = usePlayground((s) => s.setBody);
  const setBearerToken = usePlayground((s) => s.setBearerToken);
  const resetCurrent = usePlayground((s) => s.resetCurrent);

  if (!last) {
    return <EmptyState title="Send a request to see the response" />;
  }

  const opId = selected ?? "unknown";
  const opHistory = history[opId] ?? [];
  const contentType = last.headers["content-type"] ?? "";
  const isJson = contentType.includes("application/json");
  const sortedHeaders = Object.entries(last.headers).sort(([a], [b]) => a.localeCompare(b));

  return (
    <div className="flex flex-col h-full">
      <div className="px-3 py-2 border-b border-slate-800 flex items-center gap-3 text-xs">
        <StatusBadge status={last.status} />
        <span className="font-mono text-slate-400">{last.statusText}</span>
        <span className="font-mono text-slate-600">·</span>
        <span className="font-mono text-slate-400">{last.durationMs}ms</span>
        <span className="font-mono text-slate-600">·</span>
        <span className="font-mono text-slate-400">{last.sizeBytes}b</span>
        {last.error && (
          <span className="ml-auto font-mono text-rose-300 text-[10px]">
            {last.error}
          </span>
        )}
      </div>
      <Tabs
        tabs={[
          {
            id: "body",
            label: "Body",
            content: isJson
              ? <JsonView text={last.bodyText} />
              : <pre className="font-mono text-xs text-slate-300 whitespace-pre-wrap break-all">{last.bodyText}</pre>,
          },
          {
            id: "headers",
            label: "Headers",
            content: (
              <div className="font-mono text-xs space-y-0.5">
                {sortedHeaders.map(([k, v]) => (
                  <div key={k} className="flex gap-2">
                    <span className="text-slate-500">{k}:</span>
                    <span className="text-slate-300 break-all">{v}</span>
                  </div>
                ))}
              </div>
            ),
          },
          {
            id: "history",
            label: `History (${opHistory.length})`,
            content: opHistory.length === 0
              ? <EmptyState title="No history yet" />
              : (
                <div className="space-y-1">
                  {[...opHistory].reverse().map((r, i) => (
                    <HistoryRow
                      key={opHistory.length - 1 - i}
                      record={r}
                      onRestore={() => {
                        setMethod(r.request.method);
                        setUrl(r.request.url);
                        resetCurrent({
                          method: r.request.method,
                          url: r.request.url,
                          params: r.request.params,
                          headers: r.request.headers,
                          body: r.request.body,
                          bearerToken: r.request.bearerToken,
                        });
                        // After resetCurrent, the param/header/bearer keys
                        // are merged. The store's setters handle diffs but
                        // for restored values we set them once.
                      }}
                    />
                  ))}
                </div>
              ),
          },
          {
            id: "curl",
            label: "cURL",
            content: (
              <pre className="font-mono text-xs text-slate-300 whitespace-pre-wrap break-all">
                {toCurl(last.request)}
              </pre>
            ),
          },
        ]}
      />
    </div>
  );
}
```

- [ ] **Step 3: Verify TypeScript compiles**

Run: `cd plugins/umbral-playground/frontend && npx tsc --noEmit 2>&1 | tail -20`

Expected: no errors. (There may be unused import warnings for `setMethod`, `setParam`, `setHeader`, `setBody`, `setBearerToken` if the restore logic only uses `resetCurrent` — remove the unused imports if your linter is strict.)

- [ ] **Step 4: Commit**

```bash
git add plugins/umbral-playground/frontend/components/
git commit -m "feat(playground): full response viewer with body, headers, history, cURL"
```

### Task 16: Hydrate history from localStorage on mount

**Files:**
- Modify: `plugins/umbral-playground/frontend/components/App.tsx`

- [ ] **Step 1: Add the hydration effect**

In `plugins/umbral-playground/frontend/components/App.tsx`, add at the top of the `App` function body (before the existing `useEffect`):

```tsx
import { loadHistory } from "../state/history";
import { useEffect } from "react";

// inside App():
useEffect(() => {
  usePlayground.setState({ history: loadHistory() });
}, []);
```

- [ ] **Step 2: Verify TypeScript compiles**

Run: `cd plugins/umbral-playground/frontend && npx tsc --noEmit 2>&1 | tail -10`

Expected: no errors.

- [ ] **Step 3: Commit**

```bash
git add plugins/umbral-playground/frontend/components/App.tsx
git commit -m "feat(playground): hydrate history from localStorage on mount"
```

### Task 17: M4 verification — full request/response loop works

This is a manual smoke test in a browser, like M3. We add it as a documented step rather than a test, because the *interactive* end-to-end is what we're verifying.

**Files:** none.

- [ ] **Step 1: Add the M4 manual check to the README**

In `plugins/umbral-playground/README.md`, replace the "Manual smoke test" section with:

```markdown
## Manual smoke test

After `cargo build -p umbral-playground` with the CLIs installed:

1. Run an example app that registers `RestPlugin`, `OpenApiPlugin`, and `PlaygroundPlugin`.
2. Open `http://localhost:<port>/api/playground/` in a browser.
3. The 3-pane shell renders: left endpoint tree, center request builder, right response viewer.
4. The left pane shows a "Loading spec..." state, then a list of endpoints grouped by tag.
5. Click an endpoint; the center URL strip populates with the method and path.
6. Click "Send"; the right pane shows the response status, body, headers, and a `cURL` tab.
7. Click Send again; the History tab in the right pane shows 2 entries; click an entry to restore.
8. Reload the page; the History tab should still show the entries (loaded from localStorage).
```

- [ ] **Step 2: Commit**

```bash
git add plugins/umbral-playground/README.md
git commit -m "docs(playground): extend manual smoke test for M4 request/response flow"
```

---

## Milestone 5: Tests, persistence polish, Rust integration test

Goal: the test pyramid from the spec is in place. Pure-function tests, component tests, and a Rust integration test all pass. The history persistence is fully wired.

### Task 18: Component tests for RequestBuilder and ResponseViewer

**Files:**
- Create: `plugins/umbral-playground/frontend/vitest.config.ts`
- Create: `plugins/umbral-playground/frontend/vitest.setup.ts`
- Create: `plugins/umbral-playground/frontend/__tests__/RequestBuilder.test.tsx`
- Create: `plugins/umbral-playground/frontend/__tests__/ResponseViewer.test.tsx`

- [ ] **Step 1: Write `vitest.config.ts`**

Write to `plugins/umbral-playground/frontend/vitest.config.ts`:

```typescript
import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./vitest.setup.ts"],
  },
});
```

- [ ] **Step 2: Write `vitest.setup.ts`**

Write to `plugins/umbral-playground/frontend/vitest.setup.ts`:

```typescript
import "@testing-library/jest-dom/vitest";
```

- [ ] **Step 3: Write the `RequestBuilder.test.tsx`**

Write to `plugins/umbral-playground/frontend/__tests__/RequestBuilder.test.tsx`:

```tsx
import { describe, it, expect, beforeEach } from "vitest";
import { render, screen } from "@testing-library/react";
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

  it("populates the URL strip with the selected operation's path", () => {
    usePlayground.setState({ selectedOperationId: "get-article" });
    render(<RequestBuilder />);
    expect(screen.getByDisplayValue("/api/articles/{id}/")).toBeInTheDocument();
  });

  it("renders path-template inputs when the path has {placeholders}", () => {
    usePlayground.setState({ selectedOperationId: "get-article" });
    render(<RequestBuilder />);
    expect(screen.getByText("id")).toBeInTheDocument();
  });
});
```

- [ ] **Step 4: Write the `ResponseViewer.test.tsx`**

Write to `plugins/umbral-playground/frontend/__tests__/ResponseViewer.test.tsx`:

```tsx
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
```

- [ ] **Step 5: Run the tests**

Run: `cd plugins/umbral-playground/frontend && npx vitest run 2>&1 | tail -30`

Expected: all tests pass. If `RequestBuilder` tests fail because `useEffect` doesn't fire synchronously in jsdom, wrap the assertions in `await waitFor(...)` from `@testing-library/react`.

- [ ] **Step 6: Commit**

```bash
git add plugins/umbral-playground/frontend/__tests__/ plugins/umbral-playground/frontend/vitest.config.ts plugins/umbral-playground/frontend/vitest.setup.ts
git commit -m "test(playground): component tests for RequestBuilder and ResponseViewer"
```

### Task 19: Rust integration test for the plugin shell

**Files:**
- Create: `plugins/umbral-playground/tests/rust_integration.rs`

- [ ] **Step 1: Inspect the existing plugin's test patterns**

Read `plugins/umbral-openapi/tests/` (if it exists) or one of the other plugin test files to find the right imports and boot pattern. The plugin test convention in this project uses `tower::ServiceExt::oneshot`.

- [ ] **Step 2: Write the integration test**

Write to `plugins/umbral-playground/tests/rust_integration.rs`:

```rust
//! Integration test: the PlaygroundPlugin serves a 200 HTML shell
//! at its base path and 404s on unknown asset paths.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;
use umbral_playground::PlaygroundPlugin;

#[tokio::test]
async fn shell_returns_200_html() {
    let plugin = PlaygroundPlugin::new();
    let base = plugin
        .base_path_for_test()
        .trim_start_matches('/')
        .to_string();
    let app = plugin.routes().expect("plugin should provide a router");

    let req = Request::builder()
        .uri(format!("/{base}/"))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
    let s = String::from_utf8_lossy(&body);
    assert!(s.contains("<!doctype html>"), "expected HTML shell, got: {s}");
}

#[tokio::test]
async fn missing_asset_returns_404() {
    let plugin = PlaygroundPlugin::new();
    let base = plugin
        .base_path_for_test()
        .trim_start_matches('/')
        .to_string();
    let app = plugin.routes().expect("plugin should provide a router");

    let req = Request::builder()
        .uri(format!("/{base}/assets/does-not-exist.js"))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
```

- [ ] **Step 3: Add `base_path_for_test` to the plugin**

In `plugins/umbral-playground/src/lib.rs`, add this method to `impl PlaygroundPlugin`:

```rust
    #[doc(hidden)]
    pub fn base_path_for_test(&self) -> &str {
        &self.base_path
    }
```

This is `#[doc(hidden)]` and only present so the integration test can read the base path. Production code doesn't need it. (`Plugin::routes()` takes `&self` and the integration test could just inline a `"/api/playground"` literal, but exposing the field is clearer.)

- [ ] **Step 4: Add test dependencies if missing**

In `plugins/umbral-playground/Cargo.toml`, ensure:

```toml
[dev-dependencies]
tokio = { version = "1", features = ["macros", "rt", "rt-multi-thread"] }
tower = { version = "0.5", features = ["util"] }
http-body-util = "0.1"
```

- [ ] **Step 5: Run the integration tests**

Run: `cd crates && cargo test -p umbral-playground --test rust_integration 2>&1 | tail -20`

Expected: 2 tests pass.

- [ ] **Step 6: Run the whole umbral-playground test suite**

Run: `cd crates && cargo test -p umbral-playground 2>&1 | tail -10`

Expected: all tests pass (m2_build, rust_integration). The Vitest suite isn't run from Cargo by default — that's a v2 improvement.

- [ ] **Step 7: Commit**

```bash
git add plugins/umbral-playground/tests/rust_integration.rs plugins/umbral-playground/src/lib.rs plugins/umbral-playground/Cargo.toml
git commit -m "test(playground): rust integration test for shell + asset 404"
```

### Task 20: M5 final polish — handle the `npx vitest` from `cargo test`

**Files:**
- Create: `plugins/umbral-playground/build.rs` (modify to run vitest in test mode)
- Create: `plugins/umbral-playground/tests/run_vitest.rs` (a no-op Rust test that shells out)

- [ ] **Step 1: Decide whether to actually wire this up**

The spec says vitest is invoked from `cargo test` via `build.rs`-driven `npx vitest run`. The simplest path: don't. Instead, document `npm test` in the README. This avoids the cross-toolchain coupling (Cargo shelling out to npm), and CI can run `cargo test` and `npm test` as separate steps.

**Decision: skip this task.** Document `npm test` instead and call out that the JS tests aren't part of `cargo test`. This is a deviation from the spec §9; the spec said "documented `npm test` path for users who want to run JS tests in isolation" *as an alternative* to the cargo-driven run, so we're choosing the alternative. Note this in the M6 spec doc.

- [ ] **Step 2: Commit a comment-only commit that explains the deviation**

No code change. This task becomes a no-op, and the rationale lives in the spec's follow-up notes (Task 22 below).

### Task 21: Ensure clippy + fmt are clean across the workspace

**Files:** none (Cargo fmt/clippy only).

- [ ] **Step 1: Run `cargo fmt`**

Run: `cd crates && cargo fmt --all 2>&1 | tail -5`

Expected: no output (or only files that were reformatted).

- [ ] **Step 2: Run `cargo clippy`**

Run: `cd crates && cargo clippy -p umbral-playground --all-targets 2>&1 | tail -20`

Expected: no errors. If clippy flags unused imports or dead code in the plugin, fix them. The frontend lints are a separate concern.

- [ ] **Step 3: Run the full workspace test**

Run: `cd crates && cargo test --workspace 2>&1 | tail -20`

Expected: all tests pass. If pre-existing failures exist in unrelated crates (you may have noticed them in earlier sessions), they're not in scope here.

- [ ] **Step 4: Commit any fmt/clippy fixes**

```bash
git add -A
git commit -m "style(playground): cargo fmt + clippy cleanups"
```

Only commit if Step 1 or 2 produced changes.

---

## Milestone 6: Docs + example integration

Goal: a new user can find, install, and use the playground in under 5 minutes by following the user-facing docs.

### Task 22: User-facing MDX page

**Files:**
- Create: `documentation/docs/v0.0.1/plugins/_category_.json`
- Create: `documentation/docs/v0.0.1/plugins/playground.mdx`

- [ ] **Step 1: Inspect the existing plugins area**

Run: `ls documentation/docs/v0.0.1/plugins/ 2>/dev/null`

If the `plugins/` folder doesn't exist yet, this is the first plugin page. Create the folder.

- [ ] **Step 2: Read an existing plugin's MDX page**

Find a sibling plugin page (e.g. `admin.mdx` or `openapi.mdx` if it exists) and copy the frontmatter shape. The project rule says frontmatter is `title`, `description`, `sidebar_position`, optionally `icon`, `tab_group`, `tags`.

- [ ] **Step 3: Write `_category_.json`**

Write to `documentation/docs/v0.0.1/plugins/_category_.json`:

```json
{
  "label": "Plugins",
  "position": 5,
  "collapsed": false
}
```

(Adjust the position to match the sidebar ordering convention you found in Step 2.)

- [ ] **Step 4: Write the MDX page**

Write to `documentation/docs/v0.0.1/plugins/playground.mdx`:

```mdx
---
title: Playground
description: Interactive API playground for umbral-rest endpoints.
sidebar_position: 1
---

# Playground

`umbral-playground` mounts a Postman-style API playground at `/api/playground/`. It reads the OpenAPI spec produced by `umbral-openapi` and renders a navigable endpoint tree, a request builder, and a response viewer — all from the browser, no extra service to run.

## Quick start

```rust
use umbral_playground::PlaygroundPlugin;
use umbral_rest::RestPlugin;
use umbral_openapi::OpenApiPlugin;

let app = App::builder()
    .plugin(RestPlugin::default())
    .plugin(OpenApiPlugin::new())
    .plugin(PlaygroundPlugin::new())
    .build();
```

Open `http://localhost:8000/api/playground/` in a browser.

## Requirements

The plugin bundles a React + tailwindcss UI at compile time. To enable the full UI, install `esbuild` and `tailwindcss` once:

```bash
npm i -g esbuild tailwindcss
```

Without these, the plugin still compiles and serves a placeholder page that explains what's needed.

## What you get

- Three-pane layout: endpoint tree, request builder, response viewer.
- Per-endpoint request history persisted to localStorage.
- cURL export of any request.
- JSON syntax tree in the response viewer.
- Bearer token input in the Auth tab.

## v1 limitations

- Same-origin requests only (no CORS proxy).
- Body editor is a JSON textarea; no schema-driven form.
- Auth is a single bearer-token field; spec-driven `securitySchemes` arrive in v2.
- History is per-browser (localStorage); server-side history is a future plugin.

See [the design spec](https://github.com/umbral/umbral/blob/main/docs/superpowers/specs/2026-06-02-rest-playground-design.md) for the full design and follow-up work.
```

(Adjust the GitHub link to match the actual repo URL.)

- [ ] **Step 5: Verify the MDX renders**

If the docs site has a dev server, run it and visit the new page. If not, commit and let CI verify.

- [ ] **Step 6: Commit**

```bash
git add documentation/docs/v0.0.1/plugins/
git commit -m "docs(playground): user-facing MDX page + sidebar registration"
```

### Task 23: Extend `examples/derive-demo` to register the playground

**Files:**
- Modify: `examples/derive-demo/src/main.rs` (or wherever plugins are registered)
- Modify: `examples/derive-demo/Cargo.toml` (add the dep)

- [ ] **Step 1: Read the example's main file**

Run: `cat examples/derive-demo/src/main.rs`

Find where `RestPlugin` and `OpenApiPlugin` (or whatever REST/OpenAPI plugins it uses) are registered. The example is the demo harness for the rest+openapi integration; adding the playground here exercises all three together.

- [ ] **Step 2: Add the playground to the example's Cargo.toml**

In `examples/derive-demo/Cargo.toml`, add under `[dependencies]`:

```toml
umbral-playground = { path = "../../plugins/umbral-playground" }
```

- [ ] **Step 3: Register the plugin in the example's main**

In `examples/derive-demo/src/main.rs`, find the `App::builder()` chain and add:

```rust
.plugin(umbral_playground::PlaygroundPlugin::new())
```

The exact placement depends on the file. Insert it after the OpenApiPlugin registration.

- [ ] **Step 4: Build the example**

Run: `cd examples/derive-demo && cargo build 2>&1 | tail -10`

Expected: `Finished ...` (or one warning about the playground's CLIs if not installed — that's fine).

- [ ] **Step 5: Run the example**

Run: `cd examples/derive-demo && cargo run 2>&1 | head -20`

Then in a separate terminal, `curl http://localhost:<port>/api/playground/`. Expect a 200 HTML response.

- [ ] **Step 6: Commit**

```bash
git add examples/derive-demo/
git commit -m "feat(derive-demo): register umbral-playground alongside rest+openapi"
```

### Task 24: Final acceptance pass

**Files:** none.

- [ ] **Step 1: Run the full workspace verification**

From `crates/`:

```bash
cargo fmt --all
cargo clippy -p umbral-playground --all-targets
cargo build -p umbral-playground
cargo test -p umbral-playground
```

All four should succeed.

- [ ] **Step 2: Run the JS test suite**

```bash
cd plugins/umbral-playground/frontend
npx vitest run
```

All tests should pass.

- [ ] **Step 3: Build the example**

```bash
cd examples/derive-demo
cargo build
```

- [ ] **Step 4: Confirm the docs page is wired up**

Visit (or have CI confirm) the `playground.mdx` page renders in the sidebar.

- [ ] **Step 5: Update the README with a final "v1 done" marker**

In `plugins/umbral-playground/README.md`, add a header at the very top:

```markdown
> **Status:** v1.0 shipped. See `docs/superpowers/specs/2026-06-02-rest-playground-design.md` for the design and `docs/superpowers/plans/2026-06-02-rest-playground.md` for the build order.
```

- [ ] **Step 6: Final commit**

```bash
git add plugins/umbral-playground/README.md
git commit -m "docs(playground): mark v1 shipped in README"
```

---

## Self-review

**1. Spec coverage:**

| Spec section | Plan task(s) |
|---|---|
| §1 Goal | Whole plan |
| §2 Architecture (new crate, no umbral-openapi dep, fetch over HTTP) | Tasks 1, 2, 5, 6 |
| §2 Mount points (`/api/playground/`, `/api/playground/assets/*`) | Task 6 |
| §3 Crate layout (file tree) | Tasks 2, 4, 5, 6, 8, 9, 10 |
| §4 Build pipeline (esbuild + tailwindcss, OUT_DIR fix, degraded mode) | Task 5 |
| §4 `.gitignore` | Task 3 |
| §5 Component model (3-pane, all 12 components) | Tasks 9, 10, 14, 15 |
| §6 State (zustand store, localStorage key, debounce) | Task 8, 16 |
| §6 Why zustand | (No code, design rationale only) |
| §6 Why no React Router | (No code) |
| §6 `openapi-types` | Task 4 |
| §7 Request execution (buildFetchArgs steps 1-4) | Task 12 |
| §8 Error handling (3 categories, 4xx/5xx = valid, history caps) | Tasks 12, 13 |
| §8 CORS note | (Mentioned in Task 14, AuthTab) |
| §9 Testing (3 layers) | Tasks 7, 12, 18, 19 |
| §10 Documentation (README, MDX) | Tasks 11, 17, 22 |
| §11 Non-goals (auth-aware spec, schema forms, server history, CORS proxy, resizable panes, Playwright, pre-built binary) | All explicitly out of scope; no tasks implement them |
| §12 Milestones (6 commits) | Whole plan, structured as 6 milestone groups |
| §13 Acceptance criteria | Tasks 19, 21, 24 |

All 14 spec sections are covered. No gaps.

**2. Placeholder scan:**

- "TBD" / "TODO" / "implement later": searched, none.
- "Add appropriate error handling" / "add validation" / "handle edge cases": none — the spec's error categories are spelled out and translated into specific test cases (Task 12) and store actions (Task 13).
- "Write tests for the above" without test code: every test step has the full test code.
- "Similar to Task N": every step that involves writing code includes the full code.
- Steps describing what to do without showing how: none. Code blocks for all code.

**3. Type consistency:**

- `RequestDraft` and `ResponseRecord` defined in Task 8, used unchanged in Tasks 12, 13, 14, 15, 18. ✓
- `BuildError` variants in Task 8 stub, expanded in Task 12 to match the test cases. ✓
- `PlaygroundState` actions defined in Task 8 (`selectEndpoint`, `setMethod`, `send`, etc.) used unchanged in Task 14, 15. ✓
- `KV` interface in Task 14 used in Task 14 only. ✓
- `PlaygroundPlugin` in Task 6 has `base_path` and `at()`. Task 19 adds `base_path_for_test` — does not conflict. ✓
- `buildFetchArgs` signature in Task 8 stub (`{ ok: true, args } | { ok: false, error }`) matches the real implementation in Task 12. ✓

**One real issue found:** in Task 8, the comment in `store.ts` says "loadSpec: () => Promise<void>" and the implementation in M4 (Task 13) calls `saveHistoryDebounced(get().history)` — but `get()` is the zustand `get` function in scope at the time `send` is defined. That works because the store creator passes `(set, get)` as the second arg. Verified correct. ✓

**One real issue found:** Task 20 is a no-op. The spec's §9 language about "build.rs-driven npx vitest run" was a design choice that turned out to add more complexity than value, given that vitest is fast and CI can run two test commands. The deviation is documented in Task 20 Step 1 and reflected in the final acceptance criteria (Task 24 Step 2 is a separate `npx vitest run` invocation, not a `cargo test` invocation).

Both issues are fixed inline (Task 20's decision is recorded, and the type check above confirms no drift).

## Follow-ups (post-v1)

The following are explicit decisions made during execution that future maintainers should know about:

### Vitest is not invoked from `cargo test`

The spec (§9) called for vitest to be invoked from `cargo test` via `build.rs`-driven `npx vitest run`. We chose the documented alternative: `npm test` in `plugins/umbral-playground/frontend/`. The cross-toolchain coupling (Cargo shelling out to npm) wasn't worth the small UX win of "one command runs everything." CI runs them as two separate steps.

Affected: Task 20 (which is intentionally a no-op as a result). If you want to wire them together later, the hook would live in `plugins/umbral-playground/build.rs` (or a separate test-only build script) and would need to gracefully degrade when `npx` isn't available, mirroring the build pipeline's approach.
