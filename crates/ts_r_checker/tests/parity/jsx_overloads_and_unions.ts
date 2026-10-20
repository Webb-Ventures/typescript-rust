// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "moduleDetection": "force", "noEmit": true, "jsx": "preserve"}, "files": ["a.tsx"]}
// @filename: a.tsx
declare global {
  namespace JSX {
    interface Element { kind: "element" }
    interface ElementChildrenAttribute { children: {} }
    interface IntrinsicElements { div: {} }
  }
}

declare function Pick(p: { kind: "text"; value: string }): JSX.Element;
declare function Pick(p: { kind: "count"; value: number }): JSX.Element;

const p1 = <Pick kind="text" value="a" />;
const p2 = <Pick kind="count" value={1} />;
const p3 = <Pick kind="count" value="a" />;

type Props =
  | { numeric?: false; children: (arg: string) => void }
  | { numeric: true; children: (arg: number) => void };
declare function Render(p: Props): JSX.Element;

const r1 = <Render>{(v) => v.toUpperCase()}</Render>;
const r2 = <Render numeric>{(v) => v.toFixed()}</Render>;

function List<T>(p: { items: T[]; first: T }) { return <div />; }
const l1 = <List items={[1, 2]} first={3} />;
const l2 = <List items={[1, 2]} first="x" />;
export { p1, p2, p3, r1, r2, l1, l2 };
