// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "moduleDetection": "force", "noEmit": true, "jsx": "react"}, "files": ["a.tsx"]}
// @filename: a.tsx
declare global {
  namespace JSX {
    interface Element { kind: "element" }
    interface ElementClass { render(): Node | null }
    interface ElementChildrenAttribute { children: {} }
    interface IntrinsicAttributes { key?: string | number }
    interface IntrinsicClassAttributes<T> { ref?: T }
    interface IntrinsicElements { div: { children?: Node } }
  }
}
type Node = string | number | JSX.Element | null | undefined | Iterable<Node>;
declare namespace React {
  function createElement(...args: any[]): JSX.Element;
  const Fragment: (props: { children?: Node }) => Node;
}

// Fragment children are checked against `React.Fragment`'s props, reported at `<>`.
const f = () => "s";
const bad = <>{f}</>;
const bad2 = <>a{f}</>;
const ok = <>a{1}<div /></>;
const ok2 = <></>;
export {};
