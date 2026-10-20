#!/usr/bin/env node
// The AI policy check (AI_POLICY.md): a pull request that credits a model other than Anthropic's,
// OpenAI's or Meta's Muse, anywhere in its title, description, branch name, author, commit
// messages, commit authors or co-author trailers, is closed with a comment. A maintainer
// can reopen it and add the `ai-policy:exempt` label for a false match.
//
// Runs from ai-policy.yml on pull_request_target with the base branch checked out: it reads the
// pull request through the API and never runs the pull request's code.
import { fileURLToPath } from 'node:url';
import fs from 'node:fs';

// Models and agents outside the allowed list, matched as whole words ignoring case (a digit or
// a dash may follow: "qwen2.5", "gemini-2.5-pro"). Allowed: Claude (Anthropic), GPT, o-series,
// Codex and ChatGPT (OpenAI), Muse (Meta).
export const DENIED = [
  ['Gemini / Gemma / Jules (Google)', /(?<![a-z0-9])(gemini|gemma|codegemma|bard|google-labs-jules)(?![a-z])/i],
  ['DeepSeek', /(?<![a-z0-9])deepseek(?![a-z])/i],
  ['Mistral', /(?<![a-z0-9])(mistral|mixtral|codestral|devstral|magistral|ministral|pixtral)(?![a-z])/i],
  ['Qwen (Alibaba)', /(?<![a-z0-9])(qwen|qwq|codeqwen)(?![a-z])/i],
  ['Grok (xAI)', /(?<![a-z0-9])(grok|x\.ai|xai-org)(?![a-z])/i],
  ['Llama (Meta, not Muse)', /(?<![a-z0-9])(llama|codellama)(?![a-z])/i],
  ['Kimi / Moonshot', /(?<![a-z0-9])(kimi[- ]?k[0-9]|kimi-dev|moonshot\s?ai|moonshotai)(?![a-z])/i],
  ['GLM (Zhipu)', /(?<![a-z0-9])(chatglm|glm-?[0-9]|zhipu)(?![a-z])/i],
  ['MiniMax', /(?<![a-z0-9])minimax(?![a-z])/i],
  ['Phi (Microsoft)', /(?<![a-z0-9])phi-[0-9]/i],
  ['Command R (Cohere)', /(?<![a-z0-9])(command-r|command-a|cohere)(?![a-z])/i],
  ['Other open-weight models', /(?<![a-z0-9])(starcoder|nemotron|olmo|internlm|baichuan|hunyuan|doubao|ernie-?bot|granite-?[0-9]|yi-coder|seed-coder)(?![a-z])/i],
  ['Other agents’ own models', /(?<![a-z0-9])(swe-1|composer-[0-9])(?![a-z])/i],
];

/** Every denied match across `fields` ({ where, text }[]): { model, where, match }[]. */
export function findViolations(fields) {
  const out = [];
  for (const { where, text } of fields) {
    if (!text) continue;
    for (const [model, re] of DENIED) {
      const m = re.exec(text);
      if (m) out.push({ model, where, match: m[0] });
    }
  }
  return out;
}

/** The fields of a pull request and its commits that count as attribution. */
export function attributionFields(pull, commits) {
  const fields = [
    { where: 'title', text: pull.title },
    { where: 'description', text: pull.body },
    { where: 'branch name', text: pull.head?.ref },
    { where: 'author', text: pull.user?.login },
  ];
  for (const c of commits) {
    const id = c.sha?.slice(0, 7) ?? 'commit';
    fields.push({ where: `commit ${id} message`, text: c.commit?.message });
    fields.push({ where: `commit ${id} author`, text: [c.commit?.author?.name, c.commit?.author?.email, c.author?.login].filter(Boolean).join(' ') });
    fields.push({ where: `commit ${id} committer`, text: [c.commit?.committer?.name, c.commit?.committer?.email, c.committer?.login].filter(Boolean).join(' ') });
  }
  return fields;
}

const MARK = '<!-- ts-r:ai-policy -->';
export const EXEMPT = 'ai-policy:exempt';

async function main() {
  const { GH_TOKEN, REPO, PR } = process.env;
  const api = async (path, init = {}) => {
    const res = await fetch(`https://api.github.com/repos/${REPO}${path}`, {
      ...init,
      headers: { authorization: `Bearer ${GH_TOKEN}`, accept: 'application/vnd.github+json', 'content-type': 'application/json' },
    });
    if (!res.ok) throw new Error(`${init.method ?? 'GET'} ${path}: ${res.status} ${await res.text()}`);
    return res.status === 204 ? null : res.json();
  };
  const pull = await api(`/pulls/${PR}`);
  if (pull.state !== 'open') return console.log(`#${PR} is ${pull.state}`);
  if (pull.labels.some((l) => l.name === EXEMPT)) return console.log(`#${PR} is labelled ${EXEMPT}`);
  const commits = [];
  for (let page = 1; page <= 3; page++) {
    const batch = await api(`/pulls/${PR}/commits?per_page=100&page=${page}`);
    commits.push(...batch);
    if (batch.length < 100) break;
  }
  const found = findViolations(attributionFields(pull, commits));
  const summary = process.env.GITHUB_STEP_SUMMARY;
  if (found.length === 0) {
    console.log(`#${PR}: no attribution to a model outside the policy`);
    if (summary) fs.appendFileSync(summary, `#${PR} follows the AI policy.\n`);
    return;
  }
  const lines = found.map((f) => `- **${f.model}**: \`${f.match.replace(/`/g, '')}\` in the ${f.where}`);
  const body = [
    MARK,
    'Thanks for the contribution. This pull request credits an AI model that this project does not accept work from, so it has been closed automatically:',
    '',
    ...lines,
    '',
    'ts-r accepts AI-assisted work only from frontier models: Anthropic’s Claude, OpenAI’s models (GPT, o-series, Codex), and Meta’s Muse. See [AI_POLICY.md](../blob/main/AI_POLICY.md).',
    '',
    'If this is a false match (a file or word that only looks like a model name), say so here and a maintainer can reopen it with the `ai-policy:exempt` label.',
  ].join('\n');
  await api(`/issues/${PR}/comments`, { method: 'POST', body: JSON.stringify({ body }) });
  await api(`/issues/${PR}/labels`, { method: 'POST', body: JSON.stringify({ labels: ['ai-policy:closed'] }) }).catch(() => {});
  await api(`/pulls/${PR}`, { method: 'PATCH', body: JSON.stringify({ state: 'closed' }) });
  console.log(`#${PR} closed:\n${lines.join('\n')}`);
  if (summary) fs.appendFileSync(summary, `#${PR} was closed under the AI policy:\n\n${lines.join('\n')}\n`);
}

if (process.argv[1] && fileURLToPath(import.meta.url) === fs.realpathSync(process.argv[1])) {
  main().catch((error) => {
    console.error(error.message);
    process.exit(1);
  });
}
