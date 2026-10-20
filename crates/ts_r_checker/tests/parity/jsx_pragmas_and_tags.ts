// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "moduleDetection": "force", "noEmit": true, "jsx": "react"}, "files": ["g.d.ts", "a.tsx", "b.tsx", "c.tsx", "d.tsx"]}
// @filename: g.d.ts
declare namespace JSX {
  interface Element { e: 1 }
  interface IntrinsicElements { div: { id?: string }; span: {} }
  type ElementType = "div" | ((p: any) => Element | null);
}
declare namespace React {
  function createElement(...a: any[]): JSX.Element;
}
// @filename: a.tsx
/** @jsx h */
declare function h(...a: any[]): JSX.Element;
export const a1 = <><div /></>;
// @filename: b.tsx
/** @jsx h
 * @jsxfrag Frag */
declare function h(...a: any[]): JSX.Element;
export const b1 = <><div /></>;
// @filename: c.tsx
declare const kids: JSX.Element[];
declare const one: JSX.Element;
declare const loose: any;
export const c1 = <div>{...kids}</div>;
export const c2 = <div>{...one}</div>;
export const c3 = <div>{...loose}</div>;
export const c4 = <span />;
export const c5 = <A:foo />;
// @filename: d.tsx
declare function Btn(p: { onClick?: (n: number) => void }): JSX.Element;
export const d1 = <Btn onClick={(n) => n.toFixed()} />;
export const d2 = <Btn onTap={(n) => n} />;
