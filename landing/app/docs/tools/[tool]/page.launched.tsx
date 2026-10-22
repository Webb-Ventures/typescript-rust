import { DocPage, docMeta } from '@/components/doc-page';
import { assertContent } from '@/lib/content/docs.mjs';

type Props = { params: Promise<{ tool: string }> };

export const dynamicParams = false;
export const generateStaticParams = () =>
  assertContent().docs.filter(d => d.slug.startsWith('tools/')).map(d => ({ tool: d.slug.slice('tools/'.length) }));

export async function generateMetadata({ params }: Props) {
  return docMeta(`tools/${(await params).tool}`);
}

export default async function ToolDoc({ params }: Props) {
  return <DocPage slug={`tools/${(await params).tool}`} />;
}
