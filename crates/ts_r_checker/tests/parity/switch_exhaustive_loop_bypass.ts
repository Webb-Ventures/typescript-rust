// The exhaustiveness of a switch is checked only when its implicit default adds a type, and
// a nested query during that check settles it as not exhaustive: the loop sees both values.
declare function isNever(n: never): boolean;

function f() {
    let foo: "aaa" | "bbb" = "aaa";
    while (true) {
        switch (foo) {
            case "aaa":
        }
        if (foo === "aaa") {
            foo = "bbb";
        }
        else if (isNever(foo)) {
            break;
        }
    }
}
function g() {
    let foo: "aaa" | "bbb" = "aaa";
    while (true) {
        if (foo === "aaa") {
            foo = "bbb";
        }
        else if (isNever(foo)) {
            break;
        }
    }
}
export {};
