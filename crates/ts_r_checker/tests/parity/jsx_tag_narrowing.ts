// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "moduleDetection": "force", "noEmit": true, "jsx": "preserve"}, "files": ["a.tsx"]}
// @filename: a.tsx
// A value tag is a narrowable reference, `<o.C />` too (Playwright's story mounts:
// `if (!Story) throw ...; <Story />`).
declare global {
  namespace JSX {
    interface Element { kind: "element" }
    interface ElementClass { render(): Element | null }
    interface IntrinsicElements { div: {} }
  }
}
type FC = (p: {}) => JSX.Element;
type CC = new (p: {}) => { render(): JSX.Element };
declare const S6: FC | CC | undefined;
declare const S7: FC | undefined;
declare const o: { C?: FC; n: { D?: FC } };
declare let S: FC | undefined;
export function mount() {
  if (S6) { <S6 />; }
  if (S7) { <S7 />; }
  if (!S7) throw new Error();
  <S7 />;
  if (o.C) { <o.C />; }
  if (o.n.D) { <o.n.D />; }
  <o.C />;
  if (S) { S = undefined; <S />; }
}
