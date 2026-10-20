// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "moduleDetection": "force", "noEmit": true, "jsx": "preserve"}, "files": ["a.tsx"]}
// @filename: a.tsx
declare global {
  namespace JSX {
    interface Element {}
    interface ElementChildrenAttribute { children: {} }
  }
}
declare function Pair(p: { children: [number, string] }): JSX.Element;
declare function Optional(p: { children?: readonly [number, string] }): JSX.Element;
declare function Union(p: { children: [number, string] | string[] }): JSX.Element;
declare function Pick(p: { children: [number, string]; kind: "pair" }): JSX.Element;
declare function Pick(p: { children: [boolean, boolean]; kind: "other" }): JSX.Element;
export const pair = <Pair>{1}{"x"}</Pair>;
export const optional = <Optional>{1}{"x"}</Optional>;
export const union = <Union>{1}{"x"}</Union>;
export const pick = <Pick kind="pair">{1}{"x"}</Pick>;
