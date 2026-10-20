// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "moduleDetection": "force", "noEmit": true, "jsx": "react-jsx", "jsxImportSource": "lib"}, "files": ["a.tsx", "b.tsx"]}
// @filename: node_modules/lib/package.json
{"name": "lib", "version": "1.0.0", "exports": {"./jsx-runtime": {"types": "./jsx-runtime.d.ts"}}}
// @filename: node_modules/lib/jsx-runtime.d.ts
export namespace JSX {
  interface Element { lib: true }
  interface IntrinsicElements { box: { size: number } }
}
export declare function jsx(type: unknown, props: unknown): JSX.Element;
export declare function jsxs(type: unknown, props: unknown): JSX.Element;
export declare const Fragment: unique symbol;
// @filename: a.tsx
function Item(p: { label: string }) { return <box size={1} />; }
export const a1 = <box size={2} />;
export const a2 = <box size="2" />;
export const a3 = <Item label="x" />;
export const a4 = <Item>child</Item>;
export const a5 = <><box size={3} /></>;
// @filename: b.tsx
/** @jsxImportSource missing */
export const b1 = <box size={1} />;
export const b2 = <box size={2} />;
