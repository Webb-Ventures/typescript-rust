function s1<T, U, K extends string, V extends { a: string }>(
    r: Record<K, U>, p: Partial<T>, pv: Partial<V>, ro: Readonly<V>, kk: { [P in K]: P }, rn: Record<K, number>,
) {
    const a1: { [key: string]: T } = r;
    const a2: { [key: string]: U } = r;
    const a3: { [key: string]: any } = r;
    const a4: { [key: string]: unknown } = p;
    const a5: { [key: string]: string } = p;
    const a6: { [key: string]: string } = kk;
    const a7: { [key: string]: number } = kk;
    const a8: { [key: number]: U } = r;
    const b2: Record<string, number> = rn;
    const b3: Record<string, string> = rn;
    const b4: { readonly [key: string]: number } = rn;
    const b5: { [key: string]: U | undefined } = r;
    const b6: { [key: string]: unknown } = r;
    const b7: object = r;
    const b8: {} = p;
}
