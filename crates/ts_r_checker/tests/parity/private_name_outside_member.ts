// TS18016 for a private name outside a class, TS1451 for one inside a class that is not
// a member name, property access or the left of `in` (plainJSGrammarErrors,
// privateNameInInExpression).
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2022", "lib": ["ES2022"], "noEmit": true }, "files": ["a.ts"] }
// @filename: a.ts
export const o = { a: 1 };
export const t = #foo in o;
export class C {
    #x = 1;
    m() {
        const y = #x;
        return #x in this && y === 1;
    }
}
