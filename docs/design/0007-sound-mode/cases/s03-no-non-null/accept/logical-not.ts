// ! as logical not and inside !== is not a non-null assertion.
function isBlank(s: string | null): boolean {
  return !s || s !== s.trim();
}
export const b: boolean = isBlank(null);
