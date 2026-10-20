// @filename: tsconfig.json
{"compilerOptions": {"noEmit": true, "allowJs": true, "checkJs": true, "strict": true, "noFallthroughCasesInSwitch": true, "target": "ES2022", "module": "ESNext"}, "files": ["a.ts", "b.js"]}
// @filename: a.ts
// noFallthroughCasesInSwitch: a non-empty clause, not the last, whose end is reachable is
// TS7029 at the clause. Empty clauses group with the next one.
declare function fail(): never;
export function f(a: number, b: boolean) {
    switch (a) {
        case 1:
        case 2:
            if (b) {
                return 1;
            }
        case 3:
            if (b) {
                return 2;
            } else {
                return 3;
            }
        case 4:
            fail();
        case 5:
            b = !b;
            break;
        case 6:
            switch (b) {
                case true:
                    a++;
                default:
                    a--;
            }
        case 7:
            throw new Error();
        default:
            a++;
    }
    return a;
}
// @filename: b.js
export function g(a, b) {
    switch (a) {
        case 10:
            if (b) {
                return b;
            }
        case 20:
            return a;
        case 30:
            a++;
    }
}
