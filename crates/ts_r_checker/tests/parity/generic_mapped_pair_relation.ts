// @strict: true
interface Thing { a: string; b: number }

function f1<T>(x: Partial<T>, y: T) {
    y = x;
}

function f2<T>(x: Partial<Thing>, y: Partial<T>) {
    y = x;
}

function f3<T>(x: Readonly<Thing>, y: Readonly<T>) {
    y = x;
}

function f4<T, U extends T>(x: { [P in keyof T]: T[P] }, y: { [P in keyof T]: U[P] }) {
    x = y;
    y = x;
}

function f5<T, U>(x: { [P in keyof T]: T[P] }, y: { [P in keyof U]: U[P] }) {
    y = x;
}

function f6<T, K extends keyof T>(x: { [P in K]: T[P] }, y: { [P in keyof T]: T[P] }) {
    x = y;
    y = x;
}

function f7<T>(x: Partial<T>, y: Required<T>) {
    x = y;
    y = x;
}

function f8<T>(y: Partial<T>) {
    y = {};
}

function f9<T>(x: Required<T>, y: T, z: Partial<T>) {
    x = y;
    x = {};
    z = y;
    y = x;
}
