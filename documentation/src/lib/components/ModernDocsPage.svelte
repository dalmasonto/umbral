<script lang="ts">
  import {
    Header,
    // Footer,
    DocLayout,
    CategoryIndex,
    HotReloadIndicator,
    DevModeBadge,
    MdxHotReload,
    MdxContent,
    NotFoundContent,
    SearchHighlight,
    SiteBanner,
    mdxComponents,
  } from 'specra/components';
  import { sidebarStore } from 'specra/stores';
  import { link } from 'specra';
  import ModernSidebar from './ModernSidebar.svelte';
  import ModernToc from './ModernToc.svelte';
  import ModernFooter from './ModernFooter.svelte';

  interface Props {
    data: any;
  }

  let { data }: Props = $props();

  let allDocsCompat: any[] = $derived(data.allDocs);
  let previousDoc = $derived(data.previous ?? undefined);
  let nextDoc = $derived(data.next ?? undefined);
  let categoryTitle = $derived(data.categoryTitle ?? undefined);
  let categoryDescription = $derived(data.categoryDescription ?? undefined);
  let sidebarOpen = $derived($sidebarStore);

  function closeSidebar() {
    sidebarStore.close();
  }
</script>

<div class="min-h-screen bg-background">
  <!-- Header -->
  <Header
    currentVersion={data.version}
    versions={data.versions}
    versionsMeta={data.versionsMeta}
    versionBanner={data.versionBanner}
    config={data.config}
    products={data.products}
    currentProduct={data.product}
  />

  <SiteBanner config={data.config} />

  <!-- Mobile Sidebar Overlay -->
  {#if sidebarOpen}
    <div
      class="lg:hidden fixed inset-0 bg-background/80 backdrop-blur-sm z-40"
      onclick={() => sidebarStore.close()}
      onkeydown={(e) => { if (e.key === 'Escape') sidebarStore.close(); }}
      role="button"
      tabindex="-1"
      aria-label="Close sidebar"
    ></div>
  {/if}

  <!-- Mobile Sidebar Drawer -->
  <div
    class="lg:hidden fixed top-0 left-0 h-full w-72 z-50 transform transition-transform duration-300 ease-in-out {sidebarOpen ? 'translate-x-0' : '-translate-x-full'}"
    style="background: var(--sidebar);"
  >
    <div class="flex flex-col h-full border-r border-border">
      <div class="shrink-0 px-4 py-4 border-b border-border">
        <a href={link('/')} class="font-semibold text-foreground">
          {data.config.site?.title || 'Documentation'}
        </a>
      </div>
      <div class="flex-1 overflow-y-auto">
        <ModernSidebar
          docs={allDocsCompat}
          version={data.version}
          product={data.product}
          config={data.config}
          activeTabGroup={data.categoryTabGroup}
          onLinkClick={closeSidebar}
        />
      </div>
    </div>
  </div>

  <!-- Main layout: same container width as navbar -->
  <div class="container mx-auto px-4 md:px-6">
    <div class="flex" style="min-height: calc(100vh - var(--header-height, 4rem));">
      <!-- Desktop Sidebar -->
      <div class="hidden lg:block w-64 shrink-0 border-l border-r border-border">
        <div
          class="overflow-y-auto"
          style="position: sticky; top: var(--header-height, 4rem); height: calc(100vh - var(--header-height, 4rem));"
        >
          <ModernSidebar
            docs={allDocsCompat}
            version={data.version}
            product={data.product}
            config={data.config}
            activeTabGroup={data.categoryTabGroup}
          />
        </div>
      </div>

      <!-- Content -->
      <main class="flex-1 min-w-0 py-8 px-4 md:px-8">
        <div class="flex flex-col gap-2">
          {#if !data.doc && data.isCategory}
            <CategoryIndex
              categoryPath={data.slug}
              version={data.version}
              product={data.product}
              allDocs={allDocsCompat}
              title={categoryTitle}
              description={categoryDescription}
              config={data.config}
            />
          {:else if data.isNotFound}
            <NotFoundContent version={data.version} />
          {:else if data.doc}
            {#if data.isCategory}
              <CategoryIndex
                categoryPath={data.slug}
                version={data.version}
                product={data.product}
                allDocs={allDocsCompat}
                title={data.doc.meta.title}
                description={data.doc.meta.description}
                config={data.config}
              />
            {:else}
              <SearchHighlight />
              <DocLayout
                meta={data.doc.meta}
                previousDoc={previousDoc}
                nextDoc={nextDoc}
                version={data.version}
                slug={data.slug}
                product={data.product}
                config={data.config}
              >
                {#if data.doc.contentNodes}
                  <MdxContent nodes={data.doc.contentNodes} components={mdxComponents} />
                {:else}
                  {@html data.doc.content}
                {/if}
              </DocLayout>
            {/if}
          {/if}

          <ModernFooter config={data.config} />
        </div>
      </main>

      <!-- Desktop TOC -->
      {#if data.doc && !data.isCategory && data.config.navigation?.showTableOfContents}
        <div class="hidden xl:block w-64 shrink-0 border-l border-r border-border">
          <div
            class="overflow-y-auto py-6 px-4"
            style="position: sticky; top: var(--header-height, 4rem); height: calc(100vh - var(--header-height, 4rem));"
          >
            <ModernToc
              items={data.toc}
              maxDepth={data.config.navigation?.tocMaxDepth ?? 3}
            />
          </div>
        </div>
      {/if}
    </div>
  </div>
</div>

<MdxHotReload />
<HotReloadIndicator />
<DevModeBadge />
