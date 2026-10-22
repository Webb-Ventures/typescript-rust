declare var x: any;
declare function isFunction(x: unknown): x is Function;
declare function isObject(x: unknown): x is Object;
declare function isAnything(x: unknown): x is {};
declare function assertFunction(x: unknown): asserts x is Function;
declare function assertObject(x: unknown): asserts x is Object;

if (isFunction(x)) {
    x.prop;
    x(1);
}

if (isObject(x)) {
    x.method();
    x();
}

if (isAnything(x)) {
    x.method();
}

function f1(y: any) {
    assertFunction(y);
    y.prop;
}

function f2(y: any) {
    assertObject(y);
    y.method();
}

function f3(y: unknown) {
    if (isObject(y)) {
        y.method();
    }
}
export {};
