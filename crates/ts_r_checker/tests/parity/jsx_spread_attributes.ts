// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "moduleDetection": "force", "noEmit": true, "jsx": "preserve"}, "files": ["a.tsx"]}
// @filename: a.tsx
declare global {
  namespace JSX {
    interface Element { e: 1 }
    interface IntrinsicAttributes { key?: string }
    interface IntrinsicElements { div: { a?: number; b?: string } }
  }
}
interface Props { x: 2; y: true; label: string; [data: string]: string | number | boolean }
declare function C(p: Props): JSX.Element;
declare function W(p: { fixed?: boolean }): JSX.Element;
declare const n: number;
declare const u: undefined;
declare const o: { a: number } | undefined;
const obj1: { x: 2 } = { x: 2 };
function g<T>(t: T) { return <div {...t} />; }
function h<T extends { a: number }>(t: T) { return <div {...t} />; }
export const all = [
  g, h,
  <div {...n} />,
  <div {...u} />,
  <div {...o} />,
  <div a={1} {...{ a: 2 }} />,
  <div a={1} b="x" {...{ a: 2 }} {...{ b: "y", a: 3 }} />,
  <C x={2} y label="a" />,
  <C {...{ x: 2, y: true, label: "a" }} />,
  <C label="a" y {...obj1} />,
  <C label="a" y x={2} data-extra={1} />,
  <C label="a" y x={2} Label="b" />,
  <W data-testid="t" />,
  <W fixed data-testid="t" />,
];
class Holder {
  make() { return <this />; }
}
export { Holder };
