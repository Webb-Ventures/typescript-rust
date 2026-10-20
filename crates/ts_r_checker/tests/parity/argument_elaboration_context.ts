// Argument elaboration keeps the parameter as contextual type:
// nested arrow returns report their literal, not the widened type.
export {};
declare function t5(arg: { a: () => "foo" }): void;
t5({ a: () => "bar" });
declare function t6(arg: () => "foo"): void;
t6(() => "bar");
declare function t7(arg: { a: () => "foo" } & { [k: string]: () => any }): void;
t7({ a: () => "bar" });
