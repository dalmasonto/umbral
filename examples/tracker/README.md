# tracker

The task-tracker app from the **"One Model, Every Surface"** tutorial. Three models
(`Project`, `Task`, `Comment`) are declared once in the `projects` plugin, and the same
registry drives migrations, a typed ORM, an admin, REST + OpenAPI + a playground, and
GraphQL — without repeating the schema.

## Layout

| Path | What it holds |
|---|---|
| `plugins/projects/src/models.rs` | The schema: `Project`, `Label`, `Task`, `Comment` + a `TaskStatus` choices enum |
| `plugins/projects/src/lib.rs` | `ProjectsPlugin` — registers the models via `Plugin::models()` |
| `src/main.rs` | The app builder: wires the plugin + Auth, Sessions, Admin, REST, OpenAPI, Playground, GraphQL, Security, Storage |
| `src/views/public.rs` | The `/` home page handler |
| `templates/` | Server-rendered pages (Tailwind, prebuilt CSS) |

## Surfaces

| URL | Served by |
|---|---|
| `/` | home page |
| `/admin/` | `AdminPlugin` — CRUD for every registered model |
| `/api/task/`, `/api/project/`, `/api/comment/`, `/api/label/` | `RestPlugin` — auto-exposed, read-only by default |
| `/api/docs` | `OpenApiPlugin` — Swagger UI (titled "Tracker API") |
| `/playground/` | `PlaygroundPlugin` — interactive REST console (mounted off `/api` on purpose) |
| `/graphql` | `GraphqlPlugin` — GraphQL endpoint + GraphiQL in dev |

## Running

```bash
cargo run -- makemigrations   # autodetect the schema from the models
cargo run -- migrate          # apply it
cargo run -- createsuperuser  # an admin login
cargo run -- dev              # http://127.0.0.1:8000  (or: cargo run -- serve)
```

## Styling

The pages use Tailwind, compiled to `static/css/app.css` and served by the StoragePlugin
at `/static`. That bundle ships **prebuilt**, so this project renders correctly with no
`npm install`. You only need Node once you edit a template and reach for a utility class
that is not already in the bundle:

```bash
cd styles
npm install
npm run build      # or: npm run watch
```

The palette lives in `styles/input.css` as CSS variables (`--accent` is the violet).

## Where to go next

- Your first app: https://dalmasonto.github.io/umbral/docs/v0.0.1/getting-started/your-first-app
- Models & the ORM: https://dalmasonto.github.io/umbral/docs/v0.0.1/orm/models
- Migrations: https://dalmasonto.github.io/umbral/docs/v0.0.1/migrations/managed-migrations
- Admin: https://dalmasonto.github.io/umbral/docs/v0.0.1/plugins/admin
- REST: https://dalmasonto.github.io/umbral/docs/v0.0.1/rest/index
- GraphQL: https://dalmasonto.github.io/umbral/docs/v0.0.1/plugins/graphql
- The Plugin trait: https://dalmasonto.github.io/umbral/docs/v0.0.1/plugins/the-plugin-trait
