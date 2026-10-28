declare namespace N {
    function f(): void;
}
namespace N {
    export function f(x?: number): void {}
}
namespace M {
    export function h(): void;
}
declare namespace M {
    function h(x: number): void;
}
namespace M {
    export function h(x?: number): void {}
}
