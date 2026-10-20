// Each overload sees the branch its parameter types reach: 'string' returns a string, 'number' a number.
function double(value: string): string;
function double(value: number): number;
function double(value: string | number): string | number {
  if (typeof value === "string") return value + value;
  return value * 2;
}
export const d: number = double(2);
