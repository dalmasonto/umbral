<script lang="ts">
  import { ChevronDown, Check } from 'lucide-svelte';
  import { browser } from '$app/environment';

  interface Option {
    id: string;
    label: string;
    icon?: string;
  }

  interface Props {
    options: Option[];
    value: string;
    onChange: (value: string) => void;
  }

  let { options, value, onChange }: Props = $props();

  let isOpen = $state(false);
  let triggerEl = $state<HTMLDivElement | null>(null);

  let selectedLabel = $derived(
    options.find((o) => o.id === value)?.label || options[0]?.label || ''
  );

  $effect(() => {
    if (!browser || !isOpen) return;

    function handleClickOutside(e: MouseEvent) {
      if (triggerEl && !triggerEl.contains(e.target as Node)) {
        isOpen = false;
      }
    }

    function handleEscape(e: KeyboardEvent) {
      if (e.key === 'Escape') isOpen = false;
    }

    document.addEventListener('click', handleClickOutside);
    document.addEventListener('keydown', handleEscape);

    return () => {
      document.removeEventListener('click', handleClickOutside);
      document.removeEventListener('keydown', handleEscape);
    };
  });

  function select(id: string) {
    onChange(id);
    isOpen = false;
  }
</script>

<div class="sidebar-select" bind:this={triggerEl}>
  <button
    class="select-trigger"
    onclick={() => (isOpen = !isOpen)}
    aria-expanded={isOpen}
    aria-haspopup="listbox"
  >
    <span class="select-value">{selectedLabel}</span>
    <ChevronDown class="select-chevron {isOpen ? 'open' : ''}" />
  </button>

  {#if isOpen}
    <div class="select-content" role="listbox">
      {#each options as option}
        <button
          class="select-item"
          class:selected={option.id === value}
          role="option"
          aria-selected={option.id === value}
          onclick={() => select(option.id)}
        >
          <span>{option.label}</span>
          {#if option.id === value}
            <Check class="select-check" />
          {/if}
        </button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .sidebar-select {
    position: relative;
    padding: 0.75rem 1rem;
    border-bottom: 1px solid var(--border);
  }

  .select-trigger {
    display: flex;
    align-items: center;
    justify-content: space-between;
    width: 100%;
    height: 2.25rem;
    padding: 0 0.75rem;
    font-size: 0.8125rem;
    font-weight: 500;
    color: var(--foreground);
    background: var(--accent);
    border: 1px solid var(--border);
    border-radius: 0.375rem;
    cursor: pointer;
    transition: border-color 0.15s, background 0.15s;
  }

  .select-trigger:hover {
    border-color: var(--muted-foreground);
  }

  .select-value {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .select-trigger :global(.select-chevron) {
    width: 0.875rem;
    height: 0.875rem;
    color: var(--muted-foreground);
    flex-shrink: 0;
    transition: transform 0.15s;
  }

  .select-trigger :global(.select-chevron.open) {
    transform: rotate(180deg);
  }

  .select-content {
    position: absolute;
    top: calc(100% - 0.25rem);
    left: 1rem;
    right: 1rem;
    z-index: 50;
    background: var(--popover);
    border: 1px solid var(--border);
    border-radius: 0.375rem;
    padding: 0.25rem;
    box-shadow: 0 4px 6px -1px rgba(0, 0, 0, 0.1), 0 2px 4px -2px rgba(0, 0, 0, 0.1);
  }

  .select-item {
    display: flex;
    align-items: center;
    justify-content: space-between;
    width: 100%;
    padding: 0.4375rem 0.5rem;
    font-size: 0.8125rem;
    color: var(--foreground);
    background: none;
    border: none;
    border-radius: 0.25rem;
    cursor: pointer;
    transition: background 0.1s;
  }

  .select-item:hover {
    background: var(--accent);
  }

  .select-item.selected {
    color: var(--primary);
    font-weight: 500;
  }

  .select-item :global(.select-check) {
    width: 0.875rem;
    height: 0.875rem;
    color: var(--primary);
  }
</style>
