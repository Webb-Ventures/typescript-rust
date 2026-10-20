// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "moduleDetection": "force", "noEmit": true, "jsx": "preserve"}, "files": ["a.tsx"]}
// @filename: a.tsx
declare global {
  namespace JSX {
    interface Element { kind: "element" }
    interface IntrinsicElements {
      div: { id?: string; tabIndex?: number; children?: unknown };
      img: { src: string; alt?: string };
    }
  }
}

const ok = <div id="a" tabIndex={1} data-x="y">text</div>;
const wrong = <div tabIndex="1" />;
const excess = <div title="t" />;
const missing = <img alt="x" />;
const unknown = <span>hi</span>;
const e: JSX.Element = ok;
export { ok, wrong, excess, missing, unknown, e };
