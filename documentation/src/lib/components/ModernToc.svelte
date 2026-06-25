<script lang="ts">
  import { browser } from '$app/environment';
  import { renderInlineCode } from 'specra';

  interface TOCItem {
    id: string;
    title: string;
    level: number;
  }

  interface Props {
    items: TOCItem[];
    maxDepth?: number;
  }

  let { items, maxDepth = 3 }: Props = $props();

  let activeId = $state('');

  let filteredItems = $derived(items.filter((item) => item.level <= maxDepth));

  $effect(() => {
    if (!browser || filteredItems.length === 0) return;

    const observer = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          if (entry.isIntersecting) {
            activeId = entry.target.id;
            break;
          }
        }
      },
      {
        rootMargin: '-80px 0px -80% 0px',
        threshold: 0,
      }
    );

    const headingElements = filteredItems
      .map((item) => document.getElementById(item.id))
      .filter(Boolean) as HTMLElement[];

    headingElements.forEach((el) => observer.observe(el));

    return () => {
      headingElements.forEach((el) => observer.unobserve(el));
      observer.disconnect();
    };
  });

  function handleClick(e: MouseEvent, id: string) {
    e.preventDefault();
    const element = document.getElementById(id);
    if (element) {
      const offset = 100;
      const elementPosition = element.getBoundingClientRect().top;
      const offsetPosition = elementPosition + window.scrollY - offset;

      window.scrollTo({
        top: offsetPosition,
        behavior: 'smooth',
      });

      window.history.replaceState(null, '', `#${id}`);
      activeId = id;
    }
  }
</script>

{#if filteredItems.length > 0}
  <nav class="modern-toc">
    <h3 class="toc-heading">On this page</h3>
    {#each filteredItems as item}
      <a
        href="#{item.id}"
        onclick={(e) => handleClick(e, item.id)}
        class="toc-link"
        class:active={activeId === item.id}
        class:nested={item.level === 3}
      >
        {@html renderInlineCode(item.title)}
      </a>
    {/each}
  </nav>
{/if}

<style>
  .modern-toc {
    font-size: 0.8125rem;
    line-height: 1.75;
  }

  .toc-heading {
    font-size: 0.6875rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.1em;
    color: var(--muted-foreground);
    margin: 0 0 0.75rem 0;
    padding: 0;
  }

  .toc-link {
    display: block;
    padding: 0.125rem 0 0.125rem 0.75rem;
    color: var(--muted-foreground);
    text-decoration: none;
    transition: color 0.15s;
    border-left: 2px solid transparent;
  }

  .toc-link:hover {
    color: var(--foreground);
  }

  .toc-link.active {
    color: var(--primary);
    border-left-color: var(--primary);
    font-weight: 500;
  }

  .toc-link.nested {
    padding-left: 1.25rem;
  }
</style>
