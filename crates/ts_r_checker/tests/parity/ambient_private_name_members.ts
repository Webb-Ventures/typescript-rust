// Declaration emit writes `#private;` untyped: no TS7008 in ambient classes.
// @filename: a.d.ts
export declare class A {
    #private;
    static #s;
    private p;
    x;
}
// @filename: b.ts
export declare class B {
    #y;
    w;
}
export class C {
    #q;
    r;
}
// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2022", "noEmit": true}, "files": ["a.d.ts", "b.ts"]}
