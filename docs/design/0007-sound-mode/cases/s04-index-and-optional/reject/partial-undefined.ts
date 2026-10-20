// Partial<T> makes keys optional, not undefined.
interface Profile {
  readonly name: string;
  readonly bio: string;
}
export const patch: Partial<Profile> = { name: undefined }; // @expect TSR0402
