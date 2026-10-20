// A branded type with a registered refinement: the validator checks 'string', then calls isEmail.
import { parse } from "@ts-r.dev/guard";
import type { Email } from "../../_shared/validators";

const raw: unknown = "ann@example.com";
export const email: Email = parse<Email>(raw);
