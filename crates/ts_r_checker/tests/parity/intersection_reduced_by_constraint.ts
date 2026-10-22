type AB = "A" | "B";
function x<T extends AB, U extends string | number, V>(y: any, z: T & string, w: T & null, v: V & string, e: T & {}) {
    let r2: never = y as T & undefined;
    let r3: never = z;
    let r4: never = w;
    let r5: "A" = y as T & undefined;
    let r6: string = w;
    let r8: never = v;
    let r9: never = e;
    let r10: never = y as U & boolean;
}
