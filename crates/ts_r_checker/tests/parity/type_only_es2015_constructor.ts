// ES2015 constructors declared only as types (lib es5) read as values: TS2585, others TS2693.
interface Promise<T> { }
interface Map<K, V> { }
interface Foo { }
const p = Promise;
const m = Map;
const f = Foo;
export { };
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "lib": ["ES5"], "module": "ESNext", "moduleResolution": "Bundler", "noEmit": true }, "files": ["a.ts"] }
