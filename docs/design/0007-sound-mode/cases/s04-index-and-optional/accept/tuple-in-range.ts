// Tuple elements inside the tuple's length are not undefined.
const pair: [number, string] = [1, "a"];
export const fixed: string = pair[0].toFixed(1) + pair[1];
