function f10<T>() {
    type K = keyof T;
    var x: { [P in keyof T]: T[P] };
    var x: { [Q in keyof T]: T[Q] };
    var x: { [R in K]: T[R] };
}

function f11<T>() {
    var x: { [P in keyof T]: T[P] };
    var x: { [P in keyof T]?: T[P] };  // Error
    var x: { readonly [P in keyof T]: T[P] };  // Error
    var x: { readonly [P in keyof T]?: T[P] };  // Error
}

function f12<T>() {
    var x: { [P in keyof T]: T[P] };
    var x: { [P in keyof T]: T[P][] };  // Error
}

function f13<T, U>() {
    var y: Partial<T>;
    var y: { [P in keyof T]?: T[P] };
    var y: { [P in keyof T]+?: T[P] };
    var z: Readonly<T>;
    var z: { +readonly [P in keyof T]: T[P] };
    var w: Record<keyof T, U>;
    var w: { [P in keyof T]: U };
    var v: { [P in keyof T as `a${P & string}`]: T[P] };
    var v: { [Q in keyof T as `a${Q & string}`]: T[Q] };
    var v: { [Q in keyof T as `b${Q & string}`]: T[Q] };
    var u: Pick<T, keyof T>;
    var u: { [P in keyof T]: T[P] };
}
