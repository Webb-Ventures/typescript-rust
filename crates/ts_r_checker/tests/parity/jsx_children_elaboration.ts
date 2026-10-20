// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "moduleDetection": "force", "noEmit": true, "jsx": "preserve"}, "files": ["a.tsx"]}
// @filename: a.tsx
declare global {
  namespace JSX {
    interface Element { e: 1 }
    interface ElementChildrenAttribute { children: {} }
    interface IntrinsicAttributes { key?: string }
    interface IntrinsicElements { div: {} }
  }
}
function A(p: { label: string }) { return <div />; }
function B(p: {}) { return <div />; }
function D(p: { children: string }) { return <div />; }
function O(p: { children?: string }) { return <div />; }
function N(p: { children: number }) { return <div />; }
function T(p: { children: [string, string] }) { return <div />; }
function L(p: { children: string[] }) { return <div />; }
function F(p: { children: (x: number) => string }) { return <div />; }
export const all = [
  <A>child</A>,
  <A label="x">child</A>,
  <B>child</B>,
  <D>{1}</D>,
  <D>a{"b"}</D>,
  <O>{1}</O>,
  <O>a{1}</O>,
  <N>{"a"}</N>,
  <N><div /></N>,
  <N>text</N>,
  <N>{1}{2}</N>,
  <T>a</T>,
  <T>{"a"}{1}</T>,
  <L>{1}</L>,
  <L>a<div /></L>,
  <F>a</F>,
  <F>{(x) => x}</F>,
  <F>{(x) => x.toFixed()}{1}</F>,
];
