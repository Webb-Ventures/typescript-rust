// @filename: tsconfig.json
{"compilerOptions": {"noEmit": true, "allowJs": true, "checkJs": true, "strict": true, "target": "ES2022", "module": "ESNext", "moduleResolution": "Bundler"}, "files": ["a.js"]}
// @filename: a.js
// Private names on an `any` value outside class bodies are TS18016; JSDoc
// accessibility tags on private names are TS18010 at the tag.
function A() {}
A.prototype.#no = 2;
/** @type {any} */
const anyValue = {};
anyValue.#x;

class B {}
B.#foo = 3;

export class C {
    /** @private */
    #a = 1;
    /** @public */
    #m() { return 1; }
    /** @protected */
    get #p() { return 1; }
    /** @private */
    b = 2;
    constructor() {
        anyValue.#a;
    }
}
