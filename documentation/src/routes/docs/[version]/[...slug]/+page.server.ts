import {
  extractTableOfContents,
  getAdjacentDocs,
  isCategoryPage,
  getCachedAllDocs,
  getCachedDocBySlug,
  getI18nConfig,
  getProducts,
} from 'specra';
import { redirect } from '@sveltejs/kit';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async ({ params }) => {
  const { version, slug: slugArray } = params;
  const slug = slugArray.replace(/\/$/, '');

  const i18nConfig = getI18nConfig();
  const slugParts = slug.split('/');
  let locale: string | undefined;
  if (i18nConfig && i18nConfig.locales.includes(slugParts[0])) {
    locale = slugParts[0];
  }

  const allDocs = await getCachedAllDocs(version, locale);

  // Multi-product: if no docs found at /docs/{version}/, redirect to default product
  if (allDocs.length === 0) {
    const products = getProducts();
    const defaultProduct = products.find(p => p.default) || products[0];
    if (defaultProduct) {
      redirect(302, `/docs/${defaultProduct.slug}/${version}/${slug}`);
    }
  }

  const isCategory = isCategoryPage(slug, allDocs);
  const doc = await getCachedDocBySlug(slug, version);

  let title = 'Page Not Found';
  let description = 'The requested documentation page could not be found.';
  let ogUrl = `/docs/${version}/${slug}`;

  if (doc) {
    title = doc.meta.title || doc.title;
    description = doc.meta.description || `Documentation for ${title}`;
  }

  // Category page without doc content
  if (!doc && isCategory) {
    const categoryDoc = allDocs.find((d) => d.slug.startsWith(slug + '/'));
    const categoryTabGroup = categoryDoc?.meta?.tab_group || categoryDoc?.categoryTabGroup;
    const categoryTitle = slug
      .split('/')
      .pop()
      ?.replace(/-/g, ' ')
      .replace(/\b\w/g, (l) => l.toUpperCase()) || 'Category';

    return {
      version,
      slug,
      isCategory: true,
      isNotFound: false,
      doc: null,
      categoryTitle,
      categoryDescription: 'Browse the documentation in this section.',
      categoryTabGroup,
      toc: [],
      previous: null,
      next: null,
      title,
      description,
      ogUrl,
    };
  }

  // Not found
  if (!doc) {
    return {
      version,
      slug,
      isCategory: false,
      isNotFound: true,
      doc: null,
      categoryTitle: null,
      categoryDescription: null,
      categoryTabGroup: undefined,
      toc: [],
      previous: null,
      next: null,
      title,
      description,
      ogUrl,
    };
  }

  // Normal doc page
  const toc = extractTableOfContents(doc.meta.content || doc.content);
  const { previous, next } = getAdjacentDocs(slug, allDocs);
  const showCategoryIndex = isCategory && !!doc;
  const matchingDoc = allDocs.find((d) => d.slug === slug);
  const currentPageTabGroup = doc.meta?.tab_group || matchingDoc?.categoryTabGroup;

  return {
    version,
    slug,
    isCategory: showCategoryIndex,
    isNotFound: false,
    doc,
    categoryTitle: null,
    categoryDescription: null,
    categoryTabGroup: currentPageTabGroup,
    toc,
    previous: previous ? { title: previous.meta.title, slug: previous.slug } : null,
    next: next ? { title: next.meta.title, slug: next.slug } : null,
    title,
    description,
    ogUrl,
  };
};
