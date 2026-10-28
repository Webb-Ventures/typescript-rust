// Without `noImplicitThis`, `this` in an object literal's methods (even with a contextual
// type or a `ThisType<X>` marker) and in a function expression assigned to a property is
// `any`. Only a `this` parameter of the contextual signature still types it.
const o = {
    a: 1,
    m() { return this.missing.deep; },
    n: function () { return this.other; },
    k() { const s: string = this.a; this.m(1, 2); },
};
const c: { a: number; m(): void } = { a: 1, m() { const s: string = this.a; } };
declare function mk<T>(o: T & ThisType<{ z: number }>): T;
mk({ m() { const s: string = this.z; } });
declare const p: { f(this: { a: number }): void };
p.f = function () { const s: string = this.a; };
const q: { a: number } = { a: 1 };
(q as any).g = function () { return this.x; };
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "noImplicitThis": false, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "moduleDetection": "force", "noEmit": true }, "files": ["a.ts"] }
