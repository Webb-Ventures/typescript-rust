// Tagged templates resolve like calls: a `TemplateStringsArray` then each substitution,
// contextually typed by the chosen signature; a constructable tag is not callable (TS2349),
// two templates in an array literal are a missing comma (TS2796).
export {};
declare function g(s: TemplateStringsArray, ...rest: boolean[]): void;
g `abc${1}def${2}ghi`;
declare function h(s: TemplateStringsArray, a: boolean, b: string): void;
h `abc${1}def${2}ghi`;
h `abc${true}`;
interface I {
    (stringParts: TemplateStringsArray, ...rest: boolean[]): I;
    member: I;
}
declare var f: I;
f `abc${1}def${2}ghi`;
f `abc${1}def${2}ghi`.member;
declare function o(s: TemplateStringsArray, n: number): number;
declare function o(s: TemplateStringsArray, n: string): string;
const r1: string = o `${1}`;
const r2: boolean = o `${true}`;
declare function cb(s: TemplateStringsArray, fn: (x: number) => void): void;
cb `${x => x.toFixed()}`;
declare function gen<T>(s: TemplateStringsArray, a: T, b: T): T;
const r3: string = gen `${1}${2}`;
class CtorTag { }
CtorTag `hello`;
var array = [
    `template string 1`
    `template string 2`
];
