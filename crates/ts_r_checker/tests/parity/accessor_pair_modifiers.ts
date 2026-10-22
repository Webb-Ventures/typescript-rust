// Accessor pairs: TS2676 when only one is abstract, TS2808 when the getter is less visible.
abstract class A {
    abstract get a(): number;
    set a(v: number) { }
    protected get b() { return 1; }
    set b(v: number) { }
    private get c() { return 1; }
    protected set c(v: number) { }
    protected get d() { return 1; }
    private set d(v: number) { }
    static get e() { return 1; }
    set e(v: number) { }
}
export { };
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "noEmit": true }, "files": ["a.ts"] }
