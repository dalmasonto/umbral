import { getCachedVersions, getCachedAllDocs, getEffectiveConfig, getI18nConfig, getVersionsMeta, getProducts, loadVersionConfig } from 'specra';
import { redirect } from '@sveltejs/kit';
import type { LayoutServerLoad } from './$types';

export const load: LayoutServerLoad = async ({ params }) => {
  const { product, version } = params;

  // Verify this is a valid product — if not, fall through to 404
  const products = getProducts();
  const matchedProduct = products.find(p => p.slug === product);
  if (!matchedProduct) {
    return {};
  }

  const i18nConfig = getI18nConfig();
  const defaultLocale = i18nConfig?.defaultLocale || 'en';

  // Block access to hidden versions — redirect to product's active version
  const currentVersionConfig = loadVersionConfig(version, product);
  if (currentVersionConfig?.hidden) {
    const config = getEffectiveConfig(version, product);
    const activeVersion = matchedProduct.config.activeVersion || config.site?.activeVersion || 'v1.0.0';
    throw redirect(302, `/docs/${product}/${activeVersion}`);
  }

  const allDocs = await getCachedAllDocs(version, defaultLocale, product);
  const versions = getCachedVersions(product);
  const config = getEffectiveConfig(version, product);
  const versionsMeta = getVersionsMeta(versions, product);

  return {
    allDocs,
    versions,
    versionsMeta,
    config,
    product,
    products,
    versionBanner: currentVersionConfig?.banner,
  };
};
