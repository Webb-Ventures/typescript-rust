function g1<T, K extends keyof T, J extends string, U extends { a: string }>(
    pk: Pick<T, K>, rec: Record<J, number>, p: Partial<T>, r: Readonly<T>, pu: Partial<U>, m: { [P in J | "c"]: number },
) {
    pk.foo;
    rec.foo;
    p.foo;
    r.foo;
    pu.a;
    pu.z;
    m.c;
    rec.toString();
    p.hasOwnProperty("x");
    const { a } = rec;
    const s = { ...rec };
    for (const k in rec) { rec[k]; }
    rec["x"];
    p["y"];
    const k2: keyof typeof rec = null!;
    Object.keys(rec);
    const c: {} = rec;
    const o: object = p;
    if ("x" in rec) { rec.x; }
    typeof rec === "object";
    rec.valueOf();
}
