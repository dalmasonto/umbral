import { redirect } from '@sveltejs/kit';
import { getCachedVersions, getCachedAllDocs, getProducts, getEffectiveConfig } from 'specra';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async ({ params }) => {
  const { version } = params;

  // Route disambiguation: check if this "version" is actually a product slug
  const products = getProducts();
  const matchedProduct = products.find(p => p.slug === version);
  if (matchedProduct) {
    // This is /docs/{product} — redirect to the product's active version
    const config = getEffectiveConfig('', version);
    const activeVersion = matchedProduct.config.activeVersion || config.site?.activeVersion || 'v1.0.0';
    redirect(302, `/docs/${version}/${activeVersion}`);
  }

  // Standard version route — check non-product docs first
  const docs = await getCachedAllDocs(version);

  if (docs.length > 0) {
    redirect(302, `/docs/${version}/${docs[0].slug}`);
  }

  // No docs found at /docs/{version}/ — redirect to default product
  const defaultProduct = products.find(p => p.default) || products[0];
  if (defaultProduct) {
    redirect(302, `/docs/${defaultProduct.slug}/${version}`);
  }

  redirect(302, '/');
};
