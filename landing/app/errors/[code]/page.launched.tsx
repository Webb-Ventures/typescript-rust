import Link from 'next/link';
import { notFound } from 'next/navigation';
import { Badges, Crumbs, DocShell, Pager, Prose } from '@/components/docs';
import { JsonLd } from '@/components/parts';
import { assertContent } from '@/lib/content/docs.mjs';
import { pageMeta } from '@/lib/meta';
import { site } from '@/lib/site';

type Props = { params: Promise<{ code: string }> };

export const dynamicParams = false;
export const generateStaticParams = () => assertContent().errors.map(e => ({ code: e.code }));

const errorByCode = (code: string) => assertContent().errors.find(e => e.code === code);
const truncate = (s: string, n: number) => (s.length > n ? s.slice(0, n - 1).replace(/\s+\S*$/, '') + '…' : s);

export async function generateMetadata({ params }: Props) {
  const e = errorByCode((await params).code);
  if (!e) return {};
  return pageMeta({
    // Search results show about 70 characters.
    title: `${e.code}: ${truncate(e.plain, 70 - e.code.length - 9)} · ts-r`,
    description: e.summary,
    path: e.path,
    absolute: true,
    markdown: true,
  });
}

export default async function ErrorPage({ params }: Props) {
  const e = errorByCode((await params).code);
  if (!e) notFound();
  return (
    <DocShell current="errors/" toc={e.toc}>
      <Crumbs items={[['Error codes', 'errors/'], [e.code, e.path]]} />
      <JsonLd
        data={{
          '@context': 'https://schema.org',
          '@type': 'TechArticle',
          headline: `${e.code}: ${e.plain}`,
          description: e.summary,
          url: `${site.url}/${e.path}`,
          inLanguage: 'en',
          isPartOf: { '@type': 'WebSite', name: site.name, url: `${site.url}/` },
          about: { '@type': 'SoftwareApplication', name: 'ts-r', applicationCategory: 'DeveloperApplication' },
        }}
      />
      <Badges items={e.badges} />
      <h1 id="title" className="code-title">{e.code}</h1>
      <p className="diag-msg"><code dangerouslySetInnerHTML={{ __html: e.messageHtml }} /></p>
      <p className="lede">{e.summary}</p>
      {e.rule && (
        <p className="small muted">
          Rule {e.rule.rule}: {e.rule.title}{e.rule.level ? ` (level: ${e.rule.level})` : ''}. Reported by ts-r-check and ts-r-lsp in files that opt in
          to sound mode. See <Link href="/docs/sound-mode/">Sound mode</Link>.
        </p>
      )}
      <Prose html={e.html} />
      <Pager prev={e.prev} next={e.next} />
    </DocShell>
  );
}
