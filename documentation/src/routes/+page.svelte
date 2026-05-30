<script lang="ts">
  import { ArrowRight, Github, Zap, BookOpen, Code } from 'lucide-svelte';
  import { Button, SiteBanner, Logo } from 'specra/components';

  let { data } = $props();
  let config = $derived(data.config);
  const docsUrl = '/docs/platform/v1.0.0/introduction/getting-started';
</script>

<svelte:head>
  <title>{config.site.title}</title>
  <meta name="description" content={config.site.description || 'Modern documentation platform'} />
</svelte:head>

<div class="min-h-screen bg-background">
  <SiteBanner {config} />

  <header class="sticky top-0 z-50 border-b bg-background/95 backdrop-blur supports-[backdrop-filter]:bg-background/60" style="border-color: var(--border);">
    <div class="max-w-7xl mx-auto flex h-14 items-center justify-between px-6">
      <a href="/" class="flex items-center gap-2">
        <Logo logo={config.site.logo} alt={config.site.title} className="w-16 object-contain" />
        <span class="font-semibold text-foreground">{config.site.title || 'Modern Docs'}</span>
      </a>
      <div class="flex items-center gap-6">
        <a href={docsUrl} class="text-sm text-muted-foreground hover:text-foreground transition-colors">Guide</a>
        <a href="/docs/platform/v1.0.0/api" class="text-sm text-muted-foreground hover:text-foreground transition-colors">API</a>
        <a href="/docs/cli/v1.0.0/getting-started/installation" class="text-sm text-muted-foreground hover:text-foreground transition-colors">CLI</a>
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
      <h1 class="text-5xl md:text-7xl font-bold tracking-tight mb-6">
        <span class="bg-gradient-to-r from-[#bd34fe] to-[#646cff] bg-clip-text text-transparent">
          Next Generation
        </span>
        <br />
        <span class="text-foreground">Documentation Platform</span>
      </h1>
      <p class="text-lg md:text-xl text-muted-foreground max-w-2xl mx-auto mb-10 leading-relaxed">
        Build beautiful, versioned documentation for your projects. Fast, flexible, and developer-friendly.
      </p>
      <div class="flex items-center justify-center gap-4">
        <Button href={docsUrl} size="lg" class="bg-[#bd34fe] hover:bg-[#a020f0] text-white">
          Get Started
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
              <Zap class="h-5 w-5 text-[#bd34fe]" />
            </div>
            <h3 class="text-base font-semibold text-foreground mb-2">Lightning Fast</h3>
            <p class="text-sm text-muted-foreground leading-relaxed">
              Pre-rendered static pages with instant navigation. Built on SvelteKit for optimal performance.
            </p>
          </div>
          <div>
            <div class="h-10 w-10 rounded-lg bg-[#bd34fe]/10 flex items-center justify-center mb-4">
              <BookOpen class="h-5 w-5 text-[#bd34fe]" />
            </div>
            <h3 class="text-base font-semibold text-foreground mb-2">Multi-Product</h3>
            <p class="text-sm text-muted-foreground leading-relaxed">
              Document multiple products and versions under one roof. Each product gets its own namespace and sidebar.
            </p>
          </div>
          <div>
            <div class="h-10 w-10 rounded-lg bg-[#bd34fe]/10 flex items-center justify-center mb-4">
              <Code class="h-5 w-5 text-[#bd34fe]" />
            </div>
            <h3 class="text-base font-semibold text-foreground mb-2">Developer First</h3>
            <p class="text-sm text-muted-foreground leading-relaxed">
              Write in MDX with interactive components, API playgrounds, and full TypeScript support.
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
