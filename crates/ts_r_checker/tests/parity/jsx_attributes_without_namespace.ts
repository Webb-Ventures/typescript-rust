// Component attributes in a program without `jsx` and without a JSX namespace (React's
// types not installed): checked like a call, children are not attributes (private-app
// apps/web, packages/ui).
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "noEmit": true }, "files": ["a.tsx"] }
// @filename: a.tsx
import { type ReactNode } from 'react'
function Section({ title, children }: { title: string; children: ReactNode }) { return null as any }
function Opt({ children }: { children?: ReactNode }) { return null as any }
function NoProps() { return null as any }
function Num(p: { n: number; s?: string }) { return null as any }
function Labelled({ children }: { label: string } & { children: (c: { id: string; 'aria-x': string }) => ReactNode }) { return children }
declare const anyv: any
export const x = <div><Section title="Credits"><p>hi</p></Section></div>
export const y = <Opt>hi</Opt>
export const a1 = <NoProps foo="x" />
export const a2 = <Num n="x" />
export const a3 = <Num n={1} s={2} />
export const a4 = <Num n={1} extra="q" />
export const a5 = <Num {...{ n: 1 }} />
export const a6 = <Num {...anyv} />
export const a7 = <Num n={anyv} bool />
export const a8 = <Num n={1} data-foo="x" />
class K { constructor(p: { k: string }) {} }
export const a9 = <K k={1} />
export const a11 = <Num />
export const a12 = <Labelled label="l">{(c: any) => c}</Labelled>
export const a13 = <Section title="t" intro={<b />} />
