// A parameter whose initializer reads the parameter itself, with an annotation (or `?`)
// that admits undefined, is circular: its declared type depends on the initializer.
declare const someCondition: boolean;
function fn1(x: number | undefined = x > 0 ? x : 0) { }
function fn2(x?: string = someCondition ? "value1" : x) { }
function f1(x: any = x) { }
function f3(x: number = x) { }
function f5(x: number | undefined = x!) { }
function f6(x: number | undefined, y: number | undefined = x ?? y) { }
function f7(x: number | undefined = x, y = x) { }
export {};
