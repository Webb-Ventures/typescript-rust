// @filename: tsconfig.json
{"compilerOptions": {"noEmit": true, "allowJs": true, "checkJs": true, "noImplicitAny": true, "target": "ES2022", "module": "ESNext", "moduleResolution": "Bundler"}, "files": ["a.d.ts", "b.js"]}
// @filename: a.d.ts
declare class A<T> { x: T; }
declare class D<T, U = string> { y: U; }
declare class Plain { z: number; }
// @filename: b.js
// A generic base class without the right number of `@extends` type arguments is TS8026.
class B extends A {}

/** @augments A */
class C extends A {}

/** @augments A<number, number, number> */
class E extends A {}

/** @augments A<number> */
class F extends A {}

/** @extends D<number> */
class G extends D {}

class H extends D {}

class I extends Plain {}
