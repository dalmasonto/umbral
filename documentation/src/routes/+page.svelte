<script lang="ts">
  import {
    ArrowRight,
    Github,
    Database,
    Code2,
    LayoutDashboard,
    Shield,
    Mail,
    Plug,
  } from 'lucide-svelte';
  import { Button, SiteBanner, Logo } from 'specra/components';

  let { data } = $props();
  let config = $derived(data.config);
  const docsUrl = '/docs/v0.0.1/about';
  const pageTitle = 'Umbra — the batteries-included Rust web framework for full-stack apps and APIs';
  const pageDescription =
    'Umbra is a Rust web framework for shipping complete web applications: ORM with managed migrations, auto-generated REST API, admin UI, auth + sessions, background tasks, email, and a plugin system. Compile-time guarantees instead of runtime hopes.';
  const pageKeywords =
    'Rust web framework, Rust ORM, Rust web framework with API, Rust admin panel, Rust REST framework, Rust Django alternative, full-stack Rust, sqlx ORM, axum framework, Rust web development, batteries-included Rust';
  let siteUrl = $derived((config?.site?.url || 'http://localhost:5173').replace(/\/$/, ''));

  let jsonLd = $derived({
    '@context': 'https://schema.org',
    '@type': 'SoftwareApplication',
    name: 'Umbra',
    applicationCategory: 'DeveloperApplication',
    operatingSystem: 'Linux, macOS, Windows',
    description: pageDescription,
    url: siteUrl,
    programmingLanguage: 'Rust',
    softwareVersion: 'v0.0.1',
    offers: { '@type': 'Offer', price: '0', priceCurrency: 'USD' },
    license: 'https://opensource.org/licenses/MIT',
  });
</script>

<svelte:head>
  <title>{pageTitle}</title>
  <meta name="description" content={pageDescription} />
  <meta name="keywords" content={pageKeywords} />
  <link rel="canonical" href={siteUrl + '/'} />

  <meta property="og:type" content="website" />
  <meta property="og:title" content={pageTitle} />
  <meta property="og:description" content={pageDescription} />
  <meta property="og:url" content={siteUrl + '/'} />
  <meta property="og:site_name" content={config?.site?.title || 'Umbra'} />

  <meta name="twitter:card" content="summary_large_image" />
  <meta name="twitter:title" content={pageTitle} />
  <meta name="twitter:description" content={pageDescription} />

  {@html `<script type="application/ld+json">${JSON.stringify(jsonLd)}</` + `script>`}
</svelte:head>

<div class="min-h-screen bg-background">
  <SiteBanner {config} />

  <header class="sticky top-0 z-50 border-b bg-background/95 backdrop-blur supports-[backdrop-filter]:bg-background/60" style="border-color: var(--border);">
    <div class="max-w-7xl mx-auto flex h-14 items-center justify-between px-6">
      <a href="/" class="flex items-center gap-2">
        <Logo logo={config.site.logo} alt={config.site.title} className="w-16 object-contain" />
        <span class="font-semibold text-foreground">{config.site.title || 'Umbra'}</span>
      </a>
      <div class="flex items-center gap-6">
        <a href={docsUrl} class="text-sm text-muted-foreground hover:text-foreground transition-colors">Docs</a>
        {#if config?.social?.github}
          <a href={config.social.github} target="_blank" rel="noopener noreferrer" aria-label="View Umbra on GitHub" class="text-muted-foreground hover:text-foreground transition-colors">
            <Github class="h-5 w-5" />
          </a>
        {/if}
      </div>
    </div>
  </header>

  <main>
    <section aria-labelledby="hero-heading" class="max-w-4xl mx-auto text-center py-24 px-6">
      <p class="text-sm uppercase tracking-widest text-muted-foreground mb-4">v0.0.1 · pre-alpha</p>
      <h1 id="hero-heading" class="text-5xl md:text-7xl font-bold tracking-tight mb-6">
        <span class="bg-gradient-to-r from-[#bd34fe] to-[#646cff] bg-clip-text text-transparent">
          Ship the whole app.
        </span>
        <br />
        <span class="text-foreground">In Rust.</span>
      </h1>
      <p class="text-lg md:text-xl text-muted-foreground max-w-2xl mx-auto mb-6 leading-relaxed">
        Umbra is a batteries-included Rust web framework: declare your data and you get
        <strong class="text-foreground font-semibold">managed migrations</strong>, a
        <strong class="text-foreground font-semibold">REST API</strong>, an
        <strong class="text-foreground font-semibold">admin UI</strong>,
        <strong class="text-foreground font-semibold">auth + sessions</strong>,
        <strong class="text-foreground font-semibold">background tasks</strong>, and
        <strong class="text-foreground font-semibold">email</strong> — with compile-time
        guarantees instead of runtime hopes.
      </p>
      <p class="text-base text-muted-foreground/80 max-w-2xl mx-auto mb-10 leading-relaxed">
        Built on <code class="font-mono text-foreground">axum</code> + <code class="font-mono text-foreground">sqlx</code> + <code class="font-mono text-foreground">sea-query</code>. PostgreSQL and SQLite first-class. Everything is a plugin.
      </p>
      <div class="flex items-center justify-center gap-4 flex-wrap">
        <Button href={docsUrl} size="lg" class="bg-[#bd34fe] hover:bg-[#a020f0] text-white">
          Get started
          <ArrowRight class="ml-2 h-4 w-4" />
        </Button>
        {#if config?.social?.github}
          <Button href={config.social.github} size="lg" variant="outline">
            <Github class="mr-2 h-4 w-4" />
            View on GitHub
          </Button>
        {/if}
      </div>
    </section>

    <section aria-labelledby="capabilities-heading" class="border-t" style="border-color: var(--border);">
      <div class="max-w-5xl mx-auto py-20 px-6">
        <h2 id="capabilities-heading" class="sr-only">What Umbra ships</h2>
        <div class="grid md:grid-cols-3 gap-10">
          <article>
            <div class="h-10 w-10 rounded-lg bg-[#bd34fe]/10 flex items-center justify-center mb-4">
              <Database class="h-5 w-5 text-[#bd34fe]" />
            </div>
            <h3 class="text-base font-semibold text-foreground mb-2">ORM + managed migrations</h3>
            <p class="text-sm text-muted-foreground leading-relaxed">
              Declare a model, change it, ship. <code class="font-mono">makemigrations</code> + <code class="font-mono">migrate</code> diff the schema for you. Typed QuerySets (<code class="font-mono">.filter</code> · <code class="font-mono">.get</code> · <code class="font-mono">.bulk_create</code>) with always-parameterized SQL.
            </p>
          </article>
          <article>
            <div class="h-10 w-10 rounded-lg bg-[#bd34fe]/10 flex items-center justify-center mb-4">
              <Code2 class="h-5 w-5 text-[#bd34fe]" />
            </div>
            <h3 class="text-base font-semibold text-foreground mb-2">Auto-generated REST API</h3>
            <p class="text-sm text-muted-foreground leading-relaxed">
              Every model gets <code class="font-mono">/api/&lt;table&gt;</code> CRUD. DRF-style <code class="font-mono">ResourceConfig</code> hides/transforms/computes fields and registers <code class="font-mono">@action</code> endpoints. OpenAPI schema for free.
            </p>
          </article>
          <article>
            <div class="h-10 w-10 rounded-lg bg-[#bd34fe]/10 flex items-center justify-center mb-4">
              <LayoutDashboard class="h-5 w-5 text-[#bd34fe]" />
            </div>
            <h3 class="text-base font-semibold text-foreground mb-2">Admin UI out of the box</h3>
            <p class="text-sm text-muted-foreground leading-relaxed">
              Auto CRUD admin for every registered model. Staff-only Basic Auth gate. Same model registry the ORM and REST surfaces read.
            </p>
          </article>
          <article>
            <div class="h-10 w-10 rounded-lg bg-[#bd34fe]/10 flex items-center justify-center mb-4">
              <Shield class="h-5 w-5 text-[#bd34fe]" />
            </div>
            <h3 class="text-base font-semibold text-foreground mb-2">Auth, sessions, security</h3>
            <p class="text-sm text-muted-foreground leading-relaxed">
              Argon2id password hashing. Anonymous sessions on every visit, fresh-token regeneration on login. Typed <code class="font-mono">User</code> / <code class="font-mono">OptionalUser</code> extractors. CSRF, autoescape, secure cookies — on by default.
            </p>
          </article>
          <article>
            <div class="h-10 w-10 rounded-lg bg-[#bd34fe]/10 flex items-center justify-center mb-4">
              <Mail class="h-5 w-5 text-[#bd34fe]" />
            </div>
            <h3 class="text-base font-semibold text-foreground mb-2">Background tasks + email</h3>
            <p class="text-sm text-muted-foreground leading-relaxed">
              DB-backed task queue (no Redis required). Pluggable email backends with multipart attachments, HTML alternatives, dev-console mode.
            </p>
          </article>
          <article>
            <div class="h-10 w-10 rounded-lg bg-[#bd34fe]/10 flex items-center justify-center mb-4">
              <Plug class="h-5 w-5 text-[#bd34fe]" />
            </div>
            <h3 class="text-base font-semibold text-foreground mb-2">Plugin-first architecture</h3>
            <p class="text-sm text-muted-foreground leading-relaxed">
              Auth, sessions, admin, tasks, REST, OpenAPI, RLS — every built-in is a plugin. Third-party plugins use the exact same <code class="font-mono">Plugin</code> trait. Dependency-inverted, statically composed.
            </p>
          </article>
        </div>
      </div>
    </section>

    <section aria-labelledby="why-heading" class="border-t" style="border-color: var(--border);">
      <div class="max-w-4xl mx-auto py-20 px-6">
        <h2 id="why-heading" class="text-2xl md:text-3xl font-bold text-foreground mb-6">Why Umbra</h2>
        <div class="space-y-5 text-muted-foreground leading-relaxed">
          <p>
            <strong class="text-foreground">Most Rust web frameworks stop at "here's a router."</strong>
            You wire up sqlx, write your own migration runner, hand-roll auth, build the admin yourself, and decide what to do about background jobs. That's a lot of code that isn't your product.
          </p>
          <p>
            <strong class="text-foreground">Umbra ships the boring stuff for you.</strong>
            Declare your model once and the framework reads it as the source of truth for the database schema, the JSON REST API, the admin UI, and the OpenAPI document. Changes flow through one autodetected migration. Auth, sessions, CSRF, secure cookies, and HTML autoescape are on by default — the easy path and the safe path are the same path.
          </p>
          <p>
            <strong class="text-foreground">And Rust's type system carries you the rest of the way.</strong>
            Nullable columns are <code class="font-mono text-foreground">Option&lt;T&gt;</code>. Backend mismatches fail at boot, not in production. SQL is always parameterized. A REST-free app compiles with zero serializer code; a Postgres-only field type refuses to start against SQLite with a clear error pointing at the model and field.
          </p>
        </div>
        <div class="mt-10 flex items-center gap-4 flex-wrap">
          <Button href={docsUrl} size="lg" class="bg-[#bd34fe] hover:bg-[#a020f0] text-white">
            Read the docs
            <ArrowRight class="ml-2 h-4 w-4" />
          </Button>
          <a href="/docs/v0.0.1/orm/models" class="text-sm text-muted-foreground hover:text-foreground transition-colors">
            See a model →
          </a>
          <a href="/docs/v0.0.1/plugins/rest" class="text-sm text-muted-foreground hover:text-foreground transition-colors">
            See the REST plugin →
          </a>
        </div>
      </div>
    </section>
  </main>

  <footer class="border-t py-8 px-6 text-center" style="border-color: var(--border);">
    <p class="text-sm text-muted-foreground">
      {config.footer?.copyright || 'Built with Specra'}
    </p>
  </footer>
</div>
