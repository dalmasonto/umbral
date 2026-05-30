<script lang="ts">
  import '../app.css';
  import { LayoutProviders } from 'specra/components';
  import type { Snippet } from 'svelte';
  import type { LayoutData } from './$types';

  let { data, children }: { data: LayoutData; children: Snippet } = $props();
</script>

<svelte:head>
  <title>{data?.config?.site?.title || 'Documentation'}</title>
  <meta name="description" content={data?.config?.site?.description || 'Modern documentation platform'} />
</svelte:head>

{#if data?.config}
  <LayoutProviders config={data.config}>
    {@render children?.()}
  </LayoutProviders>
{:else}
  {@render children?.()}
{/if}
