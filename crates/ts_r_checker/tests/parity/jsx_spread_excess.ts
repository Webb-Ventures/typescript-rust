// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "moduleDetection": "force", "noEmit": true, "jsx": "preserve"}, "files": ["a.tsx"]}
// @filename: a.tsx
declare global {
  namespace JSX {
    interface Element {}
    interface IntrinsicAttributes { key?: string }
    interface ElementChildrenAttribute { children: {} }
  }
}
interface CP { property1: string; property2: number }
interface AP { property1: string }
declare function A(p: AP): JSX.Element;
declare const props: CP;
// With a spread, the explicit attributes (and children) are still checked for excess;
// the spread's own properties are not.
const a = <A {...props} Property1 />;
const b = <A zzz {...props} />;
const c = <A {...props}>text</A>;
const d = <A {...props} />;
const e = <A {...{ property1: "", extra: 1 }} />;
export {};
