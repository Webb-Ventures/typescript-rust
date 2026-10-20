// @filename: tsconfig.json
{"compilerOptions": {"allowJs": true, "checkJs": true, "noEmit": true, "target": "ES2022", "module": "ESNext", "noImplicitOverride": true}, "files": ["a.js"]}
// @filename: a.js
// In JavaScript, `override` is a JSDoc `@override` tag and the messages say so.
export class Base {
    m() {}
    size() { return 1; }
}
export class Derived extends Base {
    m() {}
    /** @override */
    sise() { return 2; }
    /** @override */
    other() {}
}
export class Lone {
    /** @override */
    m() {}
}
