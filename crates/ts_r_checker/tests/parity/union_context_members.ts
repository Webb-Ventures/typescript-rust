// A union member whose overloads give no single contextual signature (a function type's
// generic `apply`) leaves the context to the other members, and a shorthand property
// discriminates a union context by its value's type.
declare class MyCompiler { compile(): void; }
interface PluginInstance { apply: (compiler: MyCompiler) => void; }
type PluginFunction = (this: MyCompiler, compiler: MyCompiler) => void;
export const plugins: (PluginInstance | PluginFunction)[] = [
    { apply: (compiler) => { compiler.compile(); } },
];
type ADT = {
    kind: "a";
    method(x: string): number;
} | {
    kind: "b";
    method(x: number): string;
};
declare function invoke(item: ADT): void;
const kind = "a";
invoke({
    kind,
    method(a) {
        return +a;
    },
});
