// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "moduleDetection": "force", "noEmit": true, "jsx": "preserve"}, "files": ["a.tsx"]}
// @filename: a.tsx
declare global {
  namespace JSX {
    interface Element { kind: "element" }
    interface ElementClass { render(): Element | null }
    interface ElementAttributesProperty { props: {} }
    interface ElementChildrenAttribute { children: {} }
    interface IntrinsicAttributes { key?: string | number }
    interface IntrinsicElements { div: { children?: unknown } }
  }
}

function Greeting(p: { name: string; children?: string }) {
  return <div>{p.name}</div>;
}

class Counter {
  props!: { start: number };
  render() { return <div />; }
}

const NotAComponent = { a: 1 };

const g1 = <Greeting name="x" key="k">hello</Greeting>;
const g2 = <Greeting name={1} />;
const g3 = <Greeting />;
const g4 = <Greeting name="x">{1}</Greeting>;
const c1 = <Counter start={1} />;
const c2 = <Counter start="1" />;
const c3 = <Counter />;
const bad = <NotAComponent />;
const spread = { name: "y" };
const s1 = <Greeting {...spread} />;
export { g1, g2, g3, g4, c1, c2, c3, bad, s1 };
