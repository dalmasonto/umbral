<script lang="ts">
  import { ArrowRight, Github, Layers, GitBranch, Shield } from 'lucide-svelte';
  import { Button, SiteBanner, Logo } from 'specra/components';

  let { data } = $props();
  let config = $derived(data.config);
  const docsUrl = '/docs/v0.0.1/about';
</script>

<svelte:head>
  <title>{config.site.title} — a Django-shape web framework for Rust</title>
  <meta name="description" content={config.site.description || 'A Django-shape web framework for Rust.'} />
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
          <a href={config.social.github} target="_blank" rel="noopener noreferrer" class="text-muted-foreground hover:text-foreground transition-colors">
            <Github class="h-5 w-5" />
          </a>
        {/if}
      </div>
    </div>
  </header>

  <main>
    <div class="max-w-4xl mx-auto text-center py-24 px-6">
      <p class="text-sm uppercase tracking-widest text-muted-foreground mb-4">Pre-alpha · design phase</p>
      <h1 class="text-5xl md:text-7xl font-bold tracking-tight mb-6">
        <span class="bg-gradient-to-r from-[#bd34fe] to-[#646cff] bg-clip-text text-transparent">
          Django shape.
        </span>
        <br />
        <span class="text-foreground">Rust spine.</span>
      </h1>
      <p class="text-lg md:text-xl text-muted-foreground max-w-2xl mx-auto mb-10 leading-relaxed">
        Umbra is a batteries-included web framework for Rust. Declare your data;
        get migrations, CRUD, an admin, and an optional REST API — with compile-time
        guarantees instead of runtime hopes.
      </p>
      <div class="flex items-center justify-center gap-4">
        <Button href={docsUrl} size="lg" class="bg-[#bd34fe] hover:bg-[#a020f0] text-white">
          What is Umbra?
          <ArrowRight class="ml-2 h-4 w-4" />
        </Button>
        {#if config?.social?.github}
          <Button href={config.social.github} size="lg" variant="outline">
            <Github class="mr-2 h-4 w-4" />
            View on GitHub
          </Button>
        {/if}
      </div>
    </div>

    <div class="border-t" style="border-color: var(--border);">
      <div class="max-w-5xl mx-auto py-20 px-6">
        <div class="grid md:grid-cols-3 gap-10">
          <div>
            <div class="h-10 w-10 rounded-lg bg-[#bd34fe]/10 flex items-center justify-center mb-4">
              <Layers class="h-5 w-5 text-[#bd34fe]" />
            </div>
            <h3 class="text-base font-semibold text-foreground mb-2">Thin core, plugin-heavy</h3>
            <p class="text-sm text-muted-foreground leading-relaxed">
              Auth, sessions, admin, tasks, and REST are plugins — structurally
              identical to third-party ones. A REST-free app compiles with zero
              serializer code.
            </p>
          </div>
          <div>
            <div class="h-10 w-10 rounded-lg bg-[#bd34fe]/10 flex items-center justify-center mb-4">
              <GitBranch class="h-5 w-5 text-[#bd34fe]" />
            </div>
            <h3 class="text-base font-semibold text-foreground mb-2">Managed migrations day one</h3>
            <p class="text-sm text-muted-foreground leading-relaxed">
              Declare or change a model; an autodetected migration is generated;
              <code class="font-mono">migrate</code> applies it. The declare → migrate →
              change → migrate cycle is the product, not a later feature.
            </p>
          </div>
          <div>
            <div class="h-10 w-10 rounded-lg bg-[#bd34fe]/10 flex items-center justify-center mb-4">
              <Shield class="h-5 w-5 text-[#bd34fe]" />
            </div>
            <h3 class="text-base font-semibold text-foreground mb-2">Easy path = safe path</h3>
            <p class="text-sm text-muted-foreground leading-relaxed">
              Nullable columns are <code class="font-mono">Option&lt;T&gt;</code>. Errors
              are <code class="font-mono">Result</code>. Backend mismatches fail at boot.
              SQL is always parameterized.
            </p>
          </div>
        </div>
      </div>
    </div>
  </main>

  <footer class="border-t py-8 px-6 text-center" style="border-color: var(--border);">
    <p class="text-sm text-muted-foreground">
      {config.footer?.copyright || 'Built with Specra'}
    </p>
  </footer>
</div>
