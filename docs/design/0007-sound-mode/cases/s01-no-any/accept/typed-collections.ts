// Containers created with type arguments contain no any (new Map() alone is Map<any, any> in lib).
const counts = new Map<string, number>();
counts.set("a", 1);
export const a: number = counts.get("a") ?? 0;
