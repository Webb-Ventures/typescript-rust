// @target: es2022
// @experimentalDecorators: true
// @useDefineForClassFields: false
// A class whose only decorators are on method parameters is still decorated (esbuild
// oracle: TSExperimentalDecoratorScopeIssue2147).
declare function dec(x: any): any;
let foo = 1;
class Foo {
  method1(@dec(foo) foo = 2) {}
  method2(@dec(() => foo) foo = 3) {}
}
