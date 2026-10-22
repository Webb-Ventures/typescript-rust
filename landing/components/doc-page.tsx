// One docs page (content/*.md, content/tools/*.md), used by the /docs/ routes.
import { notFound } from 'next/navigation';
import { Badges, Crumbs, DocShell, Pager, Prose } from '@/components/docs';
import { JsonLd } from '@/components/parts';
import { assertContent, stripMd } from '@/lib/content/docs.mjs';
import { pageMeta } from '@/lib/meta';
import { site } from '@/lib/site';

export const docBySlug = (slug: string) => assertContent().docs.find(d => d.slug === slug);

export function docMeta(slug: string) {
  const d = docBySlug(slug);
  if (!d) return {};
  const index = d.path === 'docs/';
  return pageMeta({
    title: index ? 'ts-r docs: getting started, tools, sound mode and status' : `${stripMd(d.title)} · ts-r docs`,
    description: d.description,
    path: d.path,
    absolute: true,
    markdown: true,
  });
}

export function DocPage({ slug }: { slug: string }) {
  const d = docBySlug(slug);
  if (!d) notFound();
  const crumbs: [string, string][] = d.path === 'docs/' ? [['Docs', 'docs/']] : [['Docs', 'docs/'], [d.label, d.path]];
  return (
    <DocShell current={d.path} toc={d.toc}>
      <Crumbs items={crumbs} />
      <JsonLd
        data={{
          '@context': 'https://schema.org',
          '@type': 'TechArticle',
          headline: stripMd(d.title),
          description: d.description,
          url: `${site.url}/${d.path}`,
          inLanguage: 'en',
          isPartOf: { '@type': 'WebSite', name: site.name, url: `${site.url}/` },
          about: { '@type': 'SoftwareApplication', name: 'ts-r', applicationCategory: 'DeveloperApplication' },
        }}
      />
      <Badges items={d.badges} />
      <h1 id="title" dangerouslySetInnerHTML={{ __html: d.titleHtml }} />
      <p className="lede">{d.description}</p>
      <Prose html={d.html} />
      <Pager prev={d.prev} next={d.next} />
    </DocShell>
  );
}

export function markdownResponse(text: string | undefined) {
  if (text === undefined) return new Response('not found', { status: 404 });
  return new Response(text, { headers: { 'content-type': 'text/markdown; charset=utf-8' } });
}
