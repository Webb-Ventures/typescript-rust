function f(o?: { color: string; width: number }, t?: [string, number]) {
    const c: string | undefined = (o || {}).color;
    const w: number | undefined = (o || {})["width"];
    const { color, width } = o || {};
    let x: string | undefined;
    ({ color: x } = o || {});
    const e = (t || {})[0];
    (o || {}).color = "a";
    (o || {})["color"] = "b";
    const s = { ...o };
    const sc = (s || {}).color;
    return [c, w, color, width, x, e, sc];
}
