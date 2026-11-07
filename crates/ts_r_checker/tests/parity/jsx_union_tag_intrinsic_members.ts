// @filename: tsconfig.json
{"compilerOptions":{"strict":true,"jsx":"preserve","noEmit":true,"lib":["es2015"],"types":[]},"files":["a.tsx"]}
// @filename: a.tsx
declare namespace JSX {
    export interface Element { e: 1 }
    export interface ElementClass { render(): any }
    export interface IntrinsicAttributes {}
    export interface IntrinsicElements { div: {} }
}
declare const G: ((p: {}) => JSX.Element) | undefined;
declare const H: ((p: {}) => JSX.Element) | null;
declare const I: ((p: {}) => JSX.Element) | "div";
declare const J: "div" | "span";
declare const K: ((p: {}) => JSX.Element) | boolean;
function f<T extends ((p: {}) => JSX.Element) | { z: 1 }>(L: T) {
    return <L />;
}
const g = <G />;
const h = <H />;
const i = <I />;
const j = <J />;
const k = <K />;
