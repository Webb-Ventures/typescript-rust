// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2022", "experimentalDecorators": true, "useDefineForClassFields": false, "noEmit": true}, "files": ["a.ts"]}
// @filename: a.ts
// Legacy decorators resolved as calls with the arguments the runtime passes.
declare function sealed<T extends { new (...args: any[]): {} }>(constructor: T): T;
declare function log(target: Object, key: string | symbol, desc: PropertyDescriptor): void;
declare function prop(target: any, key: string): void;
declare function inject(target: Object, key: string | symbol | undefined, index: number): void;
declare function Input(name?: string): any;
declare function noArgs(): ClassDecorator & MethodDecorator & PropertyDecorator;
declare function two(target: Function, index: number): void;
declare function same<T>(target: T): T;
declare function needsThree(a: any, b: any, c: any): void;
declare const anyDec: any;
class Ctor {}

@sealed
class Good {
    @prop name = "";
    @log method() {}
    @log get value() { return 1; }
    constructor(@inject dep: string) {}
    run(@inject x: number) {}
    @Input() input = "";
    @anyDec other = 1;
}

@noArgs
@Ctor
@needsThree
class Bad {
    @Input field = "";
    @noArgs method() {}
    @two other() {}
    @same again() {}
    @needsThree third = 1;
    @same static fourth = 1;
}
export {};
