// Types used by main.ts besides the ten API types.
import { Point } from "./models";
import type { Email } from "./validators";

/** A class and a refined type inside an interface. */
export interface Placed {
  at: Point;
  owner: Email;
}

export interface Chain {
  next: Chain | null;
}

export interface Env {
  GUARD_PORT: number;
  GUARD_DEBUG: boolean;
  GUARD_MODE: "dev" | "prod";
  GUARD_NAME?: string;
}

export interface Search {
  q: string;
  page?: number;
  tags: string[];
  sort: "asc" | "desc";
}

export interface Cli {
  port: number;
  verbose: boolean;
  dryRun?: boolean;
  include: string[];
  _: string[];
}

export type Nest = Nest[];

export type Flag = boolean;
