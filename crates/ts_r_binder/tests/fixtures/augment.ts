// A module file: `declare module "x"` is an augmentation, `declare global` augments globals.
import "./base";
declare module "./base" {
    interface Base { extra(): void }
}
declare global {
    interface Window { custom: string }
    var injected: number;
}
namespace Outer {
    declare module "nested" {}
}
export {};
