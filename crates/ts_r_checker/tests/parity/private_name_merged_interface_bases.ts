// A class extending a global interface's constructor derives from every class that
// interface extends (one per merged declaration): its own `#init` doesn't clash with
// the `#init` of the second base class.
import "./impl.js";

export class Cold<T> extends Obs<T> {
  #init = "own";
  make<R>(): Cold<R> {
    return new Cold<R>();
  }
}
export const o: Obs<number> = new Cold<number>();
// @filename: impl.ts
class ImplB<T> {
  #init = 1;
  b(): T | undefined {
    return undefined;
  }
}
declare global {
  interface Obs<T> extends ImplB<T> {}
}
export {};
// @filename: lib.d.ts
declare class ImplA<T> {
    #private;
    a(): T;
}
declare global {
    interface Ctor {
        new <T>(): Obs<T>;
    }
    var Obs: Ctor;
    interface Obs<T> extends ImplA<T> {
    }
}
export {};
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "noEmit": true, "types": [] }, "files": ["lib.d.ts", "a.ts", "impl.ts"] }
