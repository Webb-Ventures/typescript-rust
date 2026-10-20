// A rest parameter whose tuple type has a rest element takes the arguments from its
// position as one tuple, reported at the first of them; each element keeps its literal
// where its context has a primitive.
declare function g(...args: [...string[], number]): void;
g("a", "b", "c");
g("a", 1, 2);
g("a", "b", 3);
g(4);

declare function h(a: boolean, ...rest: [...string[], () => void]): void;
h(true, "a", "b");
h(true, "a", () => {});

declare function k(...args: [number, ...string[], boolean]): void;
k(1, "x", "y", true);
k(1, 2, true);
k("s", true);

declare function m(...args: [number, string]): void;
m(1, 2);
