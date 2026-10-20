// Relational comparisons see enum members as their primitive: numeric members compare
// with numbers, string members with strings.
const enum Mixed {
    S1 = "foo",
    S2 = "bar",
    N1 = 1000,
    N2 = 25,
}
declare var someNumber: number;
declare var someString: string;
declare const unionOfMembers: Mixed.N1 | Mixed.N2;
someNumber > Mixed.N2;
someNumber > unionOfMembers;
someString > Mixed.S1;
someString < Mixed.N1;
true < someNumber;

// Members with literal values keep their literal types next to computed members.
declare function computed(): number;
enum WithComputed {
    S1 = "foo",
    N1 = 1000,
    C1 = computed(),
}
someString > WithComputed.S1;
someNumber > WithComputed.N1;
let asString: string = WithComputed.S1;
let asNumber: number = WithComputed.N1;
let asEnum: WithComputed = WithComputed.S1;
let asMember: WithComputed.S1 = WithComputed.S1;
let fromNumber: WithComputed = 7;
let fromString: WithComputed = "foo";
let asBoolean: boolean = WithComputed.C1;
function narrow(x: WithComputed) {
    if (x === WithComputed.S1) {
        return;
    }
    switch (x) {
        case WithComputed.N1: break;
        case WithComputed.C1: break;
    }
}

// `for (let x of [])` widens the element like an initializer would (`any` without
// strictNullChecks); destructuring patterns are not widened.
function loops() {
    for (let x of []) {
        x + 1;
        (() => x + 1);
    }
    for (let { a } of []) {
        a;
    }
}

// @filename: tsconfig.json
{"compilerOptions": {"strict": false, "target": "ES2022", "lib": ["ES2022"], "noEmit": true}, "files": ["a.ts"]}
