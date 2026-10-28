export type UsefulKeys<T> = T extends any[] ? { [K in keyof T]: K }[number] : keyof T;
export type PickValue<T> = T extends ReadonlyArray<any> ? { [K in Extract<keyof T, number>]: PickValue<T[K]> }[number] : T;
type A1<T extends any[]> = { [K in keyof T]: K }[number];
type A2<T extends readonly any[]> = { [K in Extract<keyof T, number>]: T[K] }[number];
type A3<T> = { [K in keyof T]: K }[number];
type A4<T extends string[]> = { [K in keyof T]: K }["length"];
