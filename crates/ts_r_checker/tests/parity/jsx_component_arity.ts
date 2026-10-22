// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "moduleDetection": "force", "noEmit": true, "jsx": "react"}, "files": ["a.tsx"]}
// @filename: a.tsx
declare global {
  namespace JSX {
    interface Element { kind: "element" }
    interface IntrinsicElements { div: {} }
  }
}
declare namespace React {
  function createElement(type: "div", props: {}): JSX.Element;
  function createElement<P>(type: (props: P, context?: any) => JSX.Element | null, props: P): JSX.Element;
}

// A function component needing more arguments than the factory passes any component.
function Three(props: { x: number }, context: any, extra: any) { return <div />; }
function Two(props: { x: number }, context: any) { return <div />; }
function Opt(props: { x: number }, context: any, extra?: string) { return <div />; }
const a = <Three x={2} />;
const b = <Three x="no" />;
const c = <Two x={2} />;
const d = <Opt x={2} />;
export {};
