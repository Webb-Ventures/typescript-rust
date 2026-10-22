// Types for docs.mjs (plain JavaScript so Node scripts can import it too).

export type NavItem = { path: string; label: string; section: string };
export type NavLink = { path: string; label: string };
export type TocItem = { level: number; id: string; html: string };
export type Badge = { cls: string; text: string };

export type DocPage = {
  /** "docs/getting-started/" */
  path: string;
  /** Source path under content/: "tools/ts-r-dev.md" */
  rel: string;
  /** "index", "getting-started", "tools/ts-r-dev" */
  slug: string;
  title: string;
  titleHtml: string;
  label: string;
  description: string;
  section: string;
  badges: Badge[];
  html: string;
  toc: TocItem[];
  prev: NavLink | null;
  next: NavLink | null;
};

export type ErrorPage = {
  code: string;
  /** "errors/TS2322/" */
  path: string;
  planned: boolean;
  message: string;
  messageHtml: string;
  /** The message's first line with {0} placeholders as "…". */
  plain: string;
  summary: string;
  rule: { rule: string; title: string; level?: string } | null;
  badges: Badge[];
  html: string;
  toc: TocItem[];
  prev: NavLink | null;
  next: NavLink | null;
};

export type Content = {
  problems: string[];
  nav: NavItem[];
  docs: DocPage[];
  errors: ErrorPage[];
  errorStats: { catalogued: number; verified: number; selection: string };
  searchIndex: { t: string; u: string; s: string; d: string }[];
  /** Markdown copies by page path. */
  markdown: Map<string, string>;
  llms: string;
};

export const REPO: string;
export const LANDING: string;
export const ROOT: string;
export const BASE: string;
export const SECTIONS: string[];
export function stripMd(s: string): string;
export function plainMessage(m: string): string;
export function content(): Content;
export function assertContent(): Content;
