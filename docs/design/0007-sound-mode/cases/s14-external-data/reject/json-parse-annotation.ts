// JSON.parse assigned straight to a declared type.
import type { User } from "../../_shared/models";

const text = '{"id":1}';
const user: User = JSON.parse(text); // @expect TSR1401
export const id: number = user.id;
