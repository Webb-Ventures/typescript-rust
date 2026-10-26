// @target: es2015
// @module: esnext
declare class C {
    x: number;
}
namespace C {
    export const y = 1;
}
declare function f(): void;
namespace f {
    export const z = 2;
}
label: enum E {
    A,
}
outer: {
    enum F {
        B,
    }
}
