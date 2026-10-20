// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "moduleDetection": "force", "noEmit": true, "jsx": "preserve"}, "files": ["a.tsx"]}
// @filename: a.tsx
declare global {
  namespace JSX {
    interface Element {}
  }
}
declare function C(p: { "data-x"?: number }): JSX.Element;
const extra = { z: 1 };
export const before = <C data-x={1} {...extra} />;
export const after = <C {...extra} data-x={1} />;
export const ignored = <C data-other={1} />;
