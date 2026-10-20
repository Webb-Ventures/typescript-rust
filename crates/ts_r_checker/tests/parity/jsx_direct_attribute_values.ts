// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "moduleDetection": "force", "noEmit": true, "jsx": "preserve"}, "files": ["a.tsx"]}
// @filename: a.tsx
declare global {
  namespace JSX {
    interface Element {}
  }
}
declare const Outer: any, Inner: any;
declare function Typed(p: { child: JSX.Element }): JSX.Element;
export const a = <Outer child=<Inner value={unknownName} /> />;
export const b = <Outer child=<><Inner value={unknownFragment} /></> />;
export const c = <Typed child=<Inner value={unknownTyped} /> />;
export const d = <Typed child=<><Inner value={unknownTypedFragment} /></> />;
