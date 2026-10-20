declare const s: unique symbol;
declare function symbols<T>(x: { [K in keyof T & symbol]: T[K] }): T;
const symbolic = symbols({ [s]: 1 });
const expectedSymbolic: { [s]: number } = symbolic;

declare function numbers<T>(x: { [K in keyof T & number]: T[K] }): T;
const numeric = numbers({ 1: 1 });
const expectedNumeric: { 1: number } = numeric;
const key = 2;
const computed = numbers({ [key]: 'value' });
const expectedComputed: { 2: string } = computed;

declare function strings<T>(x: { [K in keyof T & string]: T[K] }): T;
const textual = strings({ '1': 1, name: 'value' });
const expectedTextual: { '1': number; name: string } = textual;
