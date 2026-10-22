import { DocPage, docMeta } from '@/components/doc-page';
import { assertContent } from '@/lib/content/docs.mjs';

type Props = { params: Promise<{ page: string }> };

export const dynamicParams = false;
export const generateStaticParams = () =>
  assertContent().docs.filter(d => d.slug !== 'index' && !d.slug.includes('/')).map(d => ({ page: d.slug }));

export async function generateMetadata({ params }: Props) {
  return docMeta((await params).page);
}

export default async function Doc({ params }: Props) {
  return <DocPage slug={(await params).page} />;
}
