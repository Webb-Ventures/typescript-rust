abstract class Base {
    abstract run(): void;
    abstract get size(): number;
}
abstract class Mid extends Base {
    abstract run(): void;
    abstract override get size(): number;
}
class Leaf extends Mid {
    run() {}
    get size() { return 1; }
}

declare const g: <T, U, V>() => Generator<T, U, V>;
declare const h: Generator<number, string, boolean>;
function* f(): Generator<number, void, string> {
    const sent = yield 1;
    const n: number = sent;
    const r = yield* h;
    const s: number = r;
    const x: number = yield* g();
    const y: boolean = yield 2;
}
export {};
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "noImplicitOverride": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "moduleDetection": "force", "noEmit": true }, "files": ["a.ts"] }
