import { getCachedVersions, getCachedAllDocs, getEffectiveConfig, getI18nConfig, getVersionsMeta, getProducts, loadVersionConfig, link } from 'specra';
import { redirect } from '@sveltejs/kit';
import type { LayoutServerLoad } from './$types';

export const load: LayoutServerLoad = async ({ params }) => {
  const { version } = params;

  // Route disambiguation: if this "version" is actually a product slug,
  // the +page.server.ts will handle the redirect. The layout still needs
  // to return data for the version case.
  const products = getProducts();
  const isProduct = products.some(p => p.slug === version);
  if (isProduct) {
    // Return minimal data - the page will redirect before rendering
    return { allDocs: [], versions: [], versionsMeta: [], config: getEffectiveConfig(''), products };
  }

  const i18nConfig = getI18nConfig();
  const defaultLocale = i18nConfig?.defaultLocale || 'en';

  // Block access to hidden versions - redirect to active version
  const currentVersionConfig = loadVersionConfig(version);
  if (currentVersionConfig?.hidden) {
    const config = getEffectiveConfig(version);
    const activeVersion = config.site?.activeVersion || 'v1.0.0';
    throw redirect(302, link(`/docs/${activeVersion}`));
  }

  let allDocs = await getCachedAllDocs(version, defaultLocale);

  // Multi-product: if no docs at /docs/{version}/, try loading from the default product
  // so the layout has data while the page redirects
  if (allDocs.length === 0) {
    const defaultProduct = products.find(p => p.default) || products[0];
    if (defaultProduct) {
      allDocs = await getCachedAllDocs(version, defaultLocale, defaultProduct.slug);
    }
  }

  const versions = getCachedVersions();
  const config = getEffectiveConfig(version);
  const versionsMeta = getVersionsMeta(versions);

  return {
    allDocs,
    versions,
    versionsMeta,
    config,
    products,
    versionBanner: currentVersionConfig?.banner,
  };
};
