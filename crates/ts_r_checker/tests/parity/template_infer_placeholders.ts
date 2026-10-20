// `infer` placeholders in template literal types: `string` when nothing is inferred,
// `never` when the source cannot match a pattern made of placeholders only.
type X4 = string extends `${string}/${string}` ? 1 : 'no';
const x4: X4 = 2;
type X2 = string extends `${infer P}/${string}` ? P : 'no';
const x2: X2 = 1;
type X5 = 'a/b' extends `${infer P}/${string}` ? P : 'no';
const x5: X5 = 1;
type X6 = string extends `${infer F}${infer R}` ? [F, R] : 'no';
const x6: X6 = 1;
type X7 = number extends `${infer F}${infer R}` ? [F, R] : 'no';
const x7: X7 = 1;
type X8 = `a${string}` extends `${infer F}${infer R}` ? [F, R] : 'no';
const x8: X8 = 1;
type X9 = string extends `${infer F}` ? [F] : 'no';
const x9: X9 = 1;

// The constraint of such a conditional is a string.
type RemoveTail<S extends string, Tail extends string> = S extends `${infer P}${Tail}` ? P : S;
type GetRouteParameter<S extends string> = RemoveTail<RemoveTail<S, `/${string}`>, `-${string}`>;
const r: RemoveTail<string, `/${string}`> = 1;

// Recursion over `string` stops.
type StringToArray<S extends string, R extends string[] = []> =
    S extends `${infer F}${infer Rest}` ? StringToArray<Rest, [...R, F]> : R;
type Len<S extends string> = StringToArray<S>['length'];
const n: Len<string> = 'x';
export {};
