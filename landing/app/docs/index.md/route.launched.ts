import { markdownResponse } from '@/components/doc-page';
import { assertContent } from '@/lib/content/docs.mjs';

export const dynamic = 'force-static';

export function GET() {
  return markdownResponse(assertContent().markdown.get('docs/'));
}
