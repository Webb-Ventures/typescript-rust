// @filename: tsconfig.json
{"compilerOptions": {"noEmit": true, "allowJs": true, "checkJs": true, "strict": true, "target": "ES2022", "module": "ESNext", "moduleResolution": "Bundler"}, "files": ["b.js", "c.ts"]}
// @filename: b.js
// @ts-nocheck
// An optional chain with a private name is a syntax error, so unchecked files report it.
export class D {
    #d;
    m() {
        this?.#d;
    }
}
// @filename: c.ts
export class E {
    e?: E;
    #f?: E;
    get(): E { return new E(); }
    m() {
        this?.#f;
        this?.e!.#f;
        this?.get().#f;
        (this?.e)?.e;
        this.#f;
    }
}
