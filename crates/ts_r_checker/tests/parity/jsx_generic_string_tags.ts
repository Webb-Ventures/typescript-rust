// @filename: tsconfig.json
{"compilerOptions":{"strict":true,"jsx":"preserve","noEmit":true,"lib":["es2015"],"types":[]},"files":["a.tsx"]}
// @filename: a.tsx
declare namespace JSX {
    export interface Element { e: 1 }
    export interface IntrinsicAttributes {}
    type LibraryManagedAttributes<C, P> = C extends { defaultProps: infer D } ? P & D : P;
    export interface IntrinsicElements { div: { a?: string }; span: { a?: string; b?: number } }
}
type Tags = "span" | "div";
const Hoc = <Tag extends Tags>(TagElement: Tag) => {
    const x = <TagElement />;
    const y = <TagElement a="" />;
    return x;
};
const Hoc3 = <Tag extends "div">(TagElement: Tag) => <TagElement />;
declare const U: "span" | "div";
const u = <U />;
declare const V: Tags;
const v = <V />;
const Hoc4 = <Tag extends string>(TagElement: Tag) => <TagElement />;
const Hoc5 = <Tag extends "div" | ((p: {}) => JSX.Element)>(TagElement: Tag) => <TagElement />;
