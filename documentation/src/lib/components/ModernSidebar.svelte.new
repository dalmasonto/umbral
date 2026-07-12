<script lang="ts">
  import { page } from '$app/stores';
  import { buildSidebarStructure, sortSidebarGroups, sortSidebarItems, resolveBadges, renderInlineCode } from 'specra';
  import type { SpecraConfig } from 'specra';
  import { SidebarBadge } from 'specra/components';
  import SidebarSelect from './SidebarSelect.svelte';

  interface DocItem {
    title: string;
    slug: string;
    filePath: string;
    section?: string;
    group?: string;
    sidebar?: string;
    sidebar_position?: number;
    categoryLabel?: string;
    categoryPosition?: number;
    categoryCollapsible?: boolean;
    categoryCollapsed?: boolean;
    categoryIcon?: string;
    categoryTabGroup?: string;
    meta: {
      icon?: string;
      tab_group?: string;
      sidebar_position?: number;
      order?: number;
      [key: string]: any;
    };
  }

  interface Props {
    docs: DocItem[];
    version: string;
    product?: string;
    config: SpecraConfig;
    activeTabGroup?: string;
    onLinkClick?: () => void;
  }

  let { docs = [], version, product, config, activeTabGroup, onLinkClick }: Props = $props();

  let docsBase = $derived(
    product && product !== '_default_'
      ? `/docs/${product}/${version}`
      : `/docs/${version}`
  );

  let pathname = $derived($page.url.pathname.replace(/\/$/, ''));

  // Tab group support
  let tabGroups = $derived(config.navigation?.tabGroups || []);
  let hasTabGroups = $derived(tabGroups.length > 0);

  let currentTabGroup = $state(activeTabGroup || '');

  // Initialize tab group from active page or default to first
  $effect(() => {
    if (!hasTabGroups) return;
    if (currentTabGroup) return;

    // Find which tab group the current page belongs to
    const currentDoc = docs.find((doc) => pathname === `${docsBase}/${doc.slug}`);
    if (currentDoc) {
      const docTab = currentDoc.meta?.tab_group || currentDoc.categoryTabGroup;
      if (docTab) {
        currentTabGroup = docTab;
        return;
      }
    }
    // Default to first tab
    currentTabGroup = tabGroups[0]?.id || '';
  });

  // Filter docs by active tab group
  let filteredDocs = $derived.by(() => {
    if (hasTabGroups && currentTabGroup) {
      return docs.filter((doc) => {
        const docTabGroup = doc.meta?.tab_group || doc.categoryTabGroup;
        if (!docTabGroup) {
          return currentTabGroup === tabGroups[0]?.id;
        }
        return docTabGroup === currentTabGroup;
      });
    }
    return docs;
  });

  let structure = $derived.by(() => {
    return buildSidebarStructure(filteredDocs);
  });

  let sortedGroups = $derived(sortSidebarGroups(structure.rootGroups));
  let sortedStandalone = $derived(sortSidebarItems(structure.standalone));

  function isActive(slug: string): boolean {
    return pathname === `${docsBase}/${slug}`;
  }

  function switchTab(tabId: string) {
    currentTabGroup = tabId;
  }
</script>

<nav class="modern-sidebar">
  <!-- Tab Group Selector -->
  {#if hasTabGroups}
    <SidebarSelect
      options={tabGroups.map((t) => ({ id: t.id, label: t.label }))}
      value={currentTabGroup}
      onChange={switchTab}
    />
  {/if}

  <!-- Standalone items -->
  {#if sortedStandalone.length > 0}
    <div class="sidebar-group">
      {#each sortedStandalone as doc}
        <a
          href="{docsBase}/{doc.slug}"
          class="sidebar-link"
          class:active={isActive(doc.slug)}
          onclick={() => onLinkClick?.()}
        >
          <span class="sidebar-link-text">{@html renderInlineCode(doc.meta.title || doc.slug)}</span>
          {#each resolveBadges(doc.meta?.badge) as badge (badge.text)}
            <SidebarBadge {badge} />
          {/each}
        </a>
      {/each}
    </div>
  {/if}

  <!-- Groups -->
  {#each sortedGroups as [key, group], groupIndex}
    <div class="sidebar-group" class:has-border={groupIndex < sortedGroups.length - 1 || sortedStandalone.length > 0}>
      <h3 class="sidebar-group-label">{@html renderInlineCode(group.label)}</h3>

      {#each sortSidebarItems(group.items) as doc}
        <a
          href="{docsBase}/{doc.slug}"
          class="sidebar-link"
          class:active={isActive(doc.slug)}
          onclick={() => onLinkClick?.()}
        >
          <span class="sidebar-link-text">{@html renderInlineCode(doc.meta.title || doc.slug)}</span>
          {#each resolveBadges(doc.meta?.badge) as badge (badge.text)}
            <SidebarBadge {badge} />
          {/each}
        </a>
      {/each}

      {#each sortSidebarGroups(group.children) as [childKey, childGroup]}
        <h4 class="sidebar-subgroup-label">{@html renderInlineCode(childGroup.label)}</h4>
        {#each sortSidebarItems(childGroup.items) as doc}
          <a
            href="{docsBase}/{doc.slug}"
            class="sidebar-link nested"
            class:active={isActive(doc.slug)}
            onclick={() => onLinkClick?.()}
          >
            <span class="sidebar-link-text">{@html renderInlineCode(doc.meta.title || doc.slug)}</span>
            {#each resolveBadges(doc.meta?.badge) as badge (badge.text)}
              <SidebarBadge {badge} />
            {/each}
          </a>
        {/each}
      {/each}
    </div>
  {/each}
</nav>

<style>
  .modern-sidebar {
    padding: 0.5rem 0;
    font-size: 0.8125rem;
    line-height: 1.75;
  }

  .sidebar-group {
    padding: 0.5rem 0;
  }

  .sidebar-group.has-border {
    border-bottom: 1px solid var(--border);
    margin-bottom: 0.25rem;
  }

  .sidebar-group-label {
    padding: 0.25rem 1.5rem;
    font-size: 0.8125rem;
    font-weight: 700;
    color: var(--foreground);
    text-transform: none;
    letter-spacing: 0;
    margin: 0;
  }

  .sidebar-subgroup-label {
    padding: 0.5rem 1.5rem 0.125rem;
    font-size: 0.75rem;
    font-weight: 600;
    color: var(--muted-foreground);
    text-transform: uppercase;
    letter-spacing: 0.05em;
    margin: 0;
  }

  .sidebar-link {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.25rem 1.5rem;
    color: var(--sidebar-foreground);
    text-decoration: none;
    transition: color 0.15s;
    border-left: 2px solid transparent;
  }

  .sidebar-link-text {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .sidebar-link:hover {
    color: var(--foreground);
  }

  .sidebar-link.active {
    color: var(--primary);
    border-left-color: var(--primary);
    font-weight: 500;
  }

  .sidebar-link.nested {
    padding-left: 2rem;
  }
</style>
