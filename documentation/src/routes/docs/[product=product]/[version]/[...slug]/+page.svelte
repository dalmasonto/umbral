<script lang="ts">
  import ModernDocsPage from '$lib/components/ModernDocsPage.svelte';
  import type { PageData } from './$types';

  let { data }: { data: PageData } = $props();
</script>

<svelte:head>
  <title>{data.title}</title>
  <meta name="description" content={data.description} />
  <meta property="og:title" content={data.title} />
  <meta property="og:description" content={data.description} />
  <meta property="og:url" content={data.ogUrl} />
  <meta property="og:type" content="article" />
  <link rel="canonical" href={data.ogUrl} />
</svelte:head>

<ModernDocsPage {data} />
