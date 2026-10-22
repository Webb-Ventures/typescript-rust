import { DocPage, docMeta } from '@/components/doc-page';

export const metadata = docMeta('index');

export default function DocsIndex() {
  return <DocPage slug="index" />;
}
