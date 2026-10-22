import Link from 'next/link';
import { Crumbs, DocShell } from '@/components/docs';
import { assertContent } from '@/lib/content/docs.mjs';
import { pageMeta } from '@/lib/meta';

export const metadata = pageMeta({
  title: 'ts-r error codes: every TS and TSR diagnostic explained',
  description:
    'One page per diagnostic code ts-r reports: tsc 7 codes, TS8xxx, ts-r’s own TS99xxx codes and sound-mode TSR codes, each with an example and a fix.',
  path: 'errors/',
  absolute: true,
});

type Err = ReturnType<typeof assertContent>['errors'][number];

function Group({ id, title, list, children }: { id: string; title: string; list: Err[]; children: React.ReactNode }) {
  if (!list.length) return null;
  return (
    <>
      <h2 id={id}><a className="anchor" href={`#${id}`} aria-label="Link to this section"><span aria-hidden="true">#</span></a>{title}</h2>
      <p>{children}</p>
      <ul className="code-list">
        {list.map(e => (
          <li key={e.code}><Link href={`/${e.path}`}><code>{e.code}</code></Link> <span>{e.plain}</span></li>
        ))}
      </ul>
    </>
  );
}

const TOC = [
  { level: 2, id: 'tsc', html: 'TypeScript codes' },
  { level: 2, id: 'ts8xxx', html: 'TS8xxx' },
  { level: 2, id: 'ts-r', html: 'TS99xxx' },
  { level: 2, id: 'tsr', html: 'Sound mode (TSR)' },
];

export default function ErrorsIndex() {
  const { errors, errorStats } = assertContent();
  const tsc = errors.filter(e => /^TS\d+$/.test(e.code) && !/^TS8\d{3}$/.test(e.code) && !/^TS99\d{3}$/.test(e.code));
  const js = errors.filter(e => /^TS8\d{3}$/.test(e.code));
  const own = errors.filter(e => /^TS99\d{3}$/.test(e.code));
  const tsr = errors.filter(e => e.planned);
  return (
    <DocShell current="errors/" toc={TOC}>
      <Crumbs items={[['Error codes', 'errors/']]} />
      <h1 id="title">Error codes</h1>
      <p className="lede">
        One page per diagnostic code: the message, what it means, a minimal example and how to fix it. Codes and messages match tsc 7.0.2, so a
        page here also explains the same error from tsc.
      </p>
      <div className="prose-doc">
        <p>
          ts-r can report {errorStats.catalogued} diagnostic codes today (<code>cargo xtask diag-codes</code> lists them from the source). This first
          pass documents {errors.length - tsr.length} of them, plus {tsr.length} sound-mode codes. {errorStats.selection}
        </p>
        <p>
          Every example was run through <code>ts-r-check</code> and is shown only if ts-r reported the page’s code; {errorStats.verified} pages have
          one. Pages without an example are codes we couldn’t trigger with a minimal input yet.
        </p>
        <Group id="tsc" title="TypeScript codes" list={tsc}>Codes ts-r reports with tsc 7’s code and message.</Group>
        <Group id="ts8xxx" title="TypeScript syntax in JavaScript files (TS8xxx)" list={js}>
          ts-r’s parser reports these like tsc when it reads <code>.js</code> files with tsc’s rules for TypeScript syntax. <code>ts-r-check</code> does
          not report them yet.
        </Group>
        <Group id="ts-r" title="ts-r’s own codes (TS99xxx)" list={own}>Limits that keep ts-r from crashing on pathological input. tsc has no equivalent.</Group>
        <Group id="tsr" title="Sound-mode codes (TSR)" list={tsr}>
          Diagnostics from <Link href="/docs/sound-mode/">sound mode</Link> (design record 0007, Accepted), reported only in files that opt in. Their
          examples are the record’s spec cases, not runs.
        </Group>
      </div>
    </DocShell>
  );
}
