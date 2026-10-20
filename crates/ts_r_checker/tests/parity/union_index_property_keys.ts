declare const key: unique symbol;
const symbolValue: { [k: symbol]: number } | { other: string } = { [key]: 1 };
const mixedValue: { [k: symbol]: number; [k: string]: string } | { other: string } = { [key]: 1 };
const numericValue: { [k: number]: number; [k: string]: number | string } | { other: string } = { 0: 1 };

interface V { value: string }
type Keys = { [k: string]: V } | { [k: number]: V };
const badUndefined: Keys | undefined = { foo: "x" };
const badNull: Keys | null = { foo: "x" };
const good: Keys | undefined = { foo: { value: "x" } };
const openObject: Keys | object = { foo: "x" };
const openEmpty: Keys | {} = { foo: "x" };
