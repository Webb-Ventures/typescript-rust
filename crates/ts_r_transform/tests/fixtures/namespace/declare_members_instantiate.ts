// @target: esnext
namespace OnlyDeclareFn {
    export declare function f(): void;
}
namespace OnlyDeclareNs {
    export declare namespace Inner {
        const x: number;
    }
}
namespace OnlyTypes {
    export interface I {}
    export declare namespace T {
        interface J {}
    }
}
