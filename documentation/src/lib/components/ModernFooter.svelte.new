<script lang="ts">
  import type { SpecraConfig } from 'specra';

  interface Props {
    config: SpecraConfig;
  }

  let { config }: Props = $props();

  let hideBranding = $derived(config.footer?.branding?.showBranding === false);
</script>

<footer class="mt-16 pt-6 pb-8 border-t border-border">
  {#if config.footer?.links && config.footer.links.length > 0}
    <div class="grid grid-cols-2 md:grid-cols-4 gap-6 mb-6">
      {#each config.footer.links as column, idx (idx)}
        <div>
          <h3 class="text-xs font-semibold text-foreground uppercase tracking-wider mb-3">{column.title}</h3>
          <ul class="space-y-1.5">
            {#each column.items as item, itemIdx (itemIdx)}
              <li>
                <a
                  href={item.href}
                  class="text-sm text-muted-foreground hover:text-foreground transition-colors"
                >
                  {item.label}
                </a>
              </li>
            {/each}
          </ul>
        </div>
      {/each}
    </div>
  {/if}

  <div class="flex flex-col md:flex-row items-center justify-between gap-3 {config.footer?.links?.length ? 'pt-6 border-t border-border' : ''}">
    {#if config.footer?.copyright}
      <p class="text-xs text-muted-foreground">
        {config.footer.copyright}
      </p>
    {/if}

    {#if !hideBranding}
      <p class="text-xs text-muted-foreground">
        Powered by
        <a
          href="https://specra-docs.com"
          target="_blank"
          rel="noopener noreferrer"
          class="font-semibold hover:text-foreground transition-colors"
        >
          Specra
        </a>
      </p>
    {/if}
  </div>
</footer>
