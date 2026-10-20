// TS2403: `typeof undefined` is `undefined`; a parameter redeclared by a `var` in the body
// is compared with the parameter (the initializer reads the parameter's type).
var undef: typeof undefined;
var undef: any;
class WithOptions {
    constructor(options?: number) {
        var options = (options || 0);
    }
}
function sameParam(p: string) {
    var p = p + 1;
}

// Assignments to functions, classes and enums are not also checked for assignability, and
// compound assignments with an invalid operand skip the write check.
enum One { A }
function fn() { }
declare var undefinedValue: undefined;
One = undefinedValue;
fn = undefinedValue;
declare var flag: boolean;
flag *= 2;
flag += 1;
declare var count: number;
count *= 2;

// `void` is not reported as possibly `undefined`; it fails the operator checks instead.
declare var nothing: void;
declare var maybe: string | void;
var compareVoid = nothing < count;
var addVoid = nothing + 1;
var negateVoid = -nothing;
var voidMember = maybe.length;

// The only member of an enum prints as the enum.
let fromOne: boolean = One.A;
enum Two { A, B }
let fromTwo: Two.A = Two.B;

// An unconstrained type parameter (or one constrained to `{}`) compares with any
// non-generic type; two unrelated type parameters do not.
function compare<T, U, S extends string, O extends {}>(t: T, u: U, s: S, o: O, b: boolean) {
    t < b;
    b < t;
    t < nothing;
    o < b;
    t == b;
    s < count;
    t < u;
    t == u;
}
