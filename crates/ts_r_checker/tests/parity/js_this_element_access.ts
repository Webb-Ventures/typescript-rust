// @filename: tsconfig.json
{"compilerOptions": {"allowJs": true, "checkJs": true, "noEmit": true, "strict": true, "target": "ES2022", "module": "ESNext"}, "files": ["a.js"]}
// @filename: a.js
// An element access on `this` that finds no property prints the class, not `this`.
export class Box {
    value = 1;
    read() {
        return this["missing"];
    }
}
