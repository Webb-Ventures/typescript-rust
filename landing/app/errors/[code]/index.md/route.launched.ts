import { markdownResponse } from '@/components/doc-page';
import { assertContent } from '@/lib/content/docs.mjs';

export const dynamic = 'force-static';
export const dynamicParams = false;
export const generateStaticParams = () => assertContent().errors.map(e => ({ code: e.code }));

export async function GET(_: Request, { params }: { params: Promise<{ code: string }> }) {
  return markdownResponse(assertContent().markdown.get(`errors/${(await params).code}/`));
}
