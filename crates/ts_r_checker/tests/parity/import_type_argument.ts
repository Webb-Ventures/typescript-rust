// `import(x)` as a type needs a string literal argument (TS1141).
type B = import(x);
type D = import(`a`);
type E = import(1);
type F = typeof import(x);
export {};
