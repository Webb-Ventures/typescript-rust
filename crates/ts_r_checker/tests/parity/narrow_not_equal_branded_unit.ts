type Left = "left";
type Right = "right" & { right: "right" };
type Either = Left | Right;
function assertNever(v: never): never {
    throw new Error("never");
}
function f(value: Either, n: (string & { brand: 1 }) | 1) {
    if (value === "left") {
        const foo: "left" = value;
    } else if (value === "right") {
        const bar: "right" = value;
    } else {
        assertNever(value);
    }
    if (n !== 1) {
        const s: string = n;
    }
    if (n !== "x") {
        const t: 1 = n;
    }
}
