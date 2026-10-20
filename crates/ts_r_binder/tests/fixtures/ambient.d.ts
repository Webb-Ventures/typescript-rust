// Ambient modules, `declare global` is only valid in modules, UMD exports.
declare module "lib" {
    export function f(): void;
    export interface Opts { a: number }
}
declare module "lib/*" {
    const value: string;
    export default value;
}
declare namespace Shape {
    interface Circle { r: number }
    const unit: Circle;
}
declare var globalVar: number;
