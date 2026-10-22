/*
 @ts-expect-error on the last line */
let a: number = "x";

/* @ts-expect-error
   not the last line */
let b: number = "x";

/*
@ts-expect-error */
let c: string = "x";

/**
 * @ts-ignore */
let d: number = "x";

// * @ts-ignore
let e: number = "x";

/// @ts-expect-error
let f: number = "x";
export { a, b, c, d, e, f };
