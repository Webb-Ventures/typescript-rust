// A global `var` does not vote on the optionality of an intersection property, nor does it
// need optional sources as a target; `in` narrows `unknown` to a record and `any` not at all;
// `T & {}` right operands of `in` may be primitives (TS2638), but `T & {}` does not derive
// from `Object`: `instanceof Object` makes it `T & Object`.
interface Win {
    onpoke?: number;
}
declare var onpoke: number;
declare var win: Win & typeof globalThis;

function f1() {
    if ("onpoke" in win) {
        const a: number | undefined = win.onpoke;
    } else {
        const b: never = win;
    }
}

function f2(x: typeof globalThis, y: Win & typeof globalThis) {
    x = y;
}

function f3(x: unknown) {
    if (x && typeof x === "object" && "a" in x) {
        const v: unknown = x.a;
        const n: number = x.a;
    }
}

function f4<T>(x: T) {
    if (x && "a" in x) {
        x.a;
    }
}

function f5<T extends object>(x: T) {
    if (x && "a" in x) {
        x.a;
    }
}

function f6<T>(x: T) {
    if (x) {
        "a" in x;
    }
}

function f7(x: any) {
    if ("a" in x) {
        const s: string = x;
    }
}

function f8<T>(x: T) {
    return x && x instanceof Object && "a" in x;
}

function f9<T>(x: T & {}) {
    if (x instanceof Object) {
        const o: Object = x;
    } else {
        const e: T = x;
    }
}
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "noUncheckedIndexedAccess": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "noEmit": true }, "files": ["a.ts"] }
