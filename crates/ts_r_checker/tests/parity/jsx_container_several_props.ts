// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "moduleDetection": "force", "noEmit": true, "jsx": "preserve"}, "files": ["a.tsx"]}
// @filename: a.tsx
declare global {
  namespace JSX {
    interface Element {}
    interface ElementAttributesProperty { pr1: any; pr2: any }
    interface ElementChildrenAttribute { c1: any; c2: any }
    interface IntrinsicElements { div: {} }
  }
}
// Several properties in a name container: TS2608 once; a class component's props are
// then its first constructor parameter, and there is no children attribute.
declare const Obj: new (n: string) => {};
const a = <Obj x={10} />;
const b = <Obj x={10} />;
const c = <div>text</div>;
export {};
