// The body is checked once per overload: with 'value: string' and with 'value: number' it returns a string.
function pad(value: string): string;
function pad(value: number): string;
function pad(value: string | number): string {
  return String(value).padStart(4, "0");
}
export const p: string = pad(7);
