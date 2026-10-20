// A 'finally' that returns overrides the 'try' block's return, so only the finalizer's value reaches the caller.
function settle(value: string): string;
function settle(value: number): number;
function settle(value: string | number): string | number {
  try {
    return 0;
  } finally {
    return value;
  }
}
export const s: string = settle("a");
