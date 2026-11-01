// Syntax highlighting for the code a review finding points at. The whole file is highlighted at
// once (so block comments and multi-line strings colour correctly) and then cut into lines, with
// the open <span>s closed at each line end and reopened on the next line.
import hljs from 'highlight.js/lib/core';
import bash from 'highlight.js/lib/languages/bash';
import c from 'highlight.js/lib/languages/c';
import cpp from 'highlight.js/lib/languages/cpp';
import csharp from 'highlight.js/lib/languages/csharp';
import css from 'highlight.js/lib/languages/css';
import go from 'highlight.js/lib/languages/go';
import java from 'highlight.js/lib/languages/java';
import javascript from 'highlight.js/lib/languages/javascript';
import json from 'highlight.js/lib/languages/json';
import kotlin from 'highlight.js/lib/languages/kotlin';
import markdown from 'highlight.js/lib/languages/markdown';
import php from 'highlight.js/lib/languages/php';
import powershell from 'highlight.js/lib/languages/powershell';
import python from 'highlight.js/lib/languages/python';
import ruby from 'highlight.js/lib/languages/ruby';
import rust from 'highlight.js/lib/languages/rust';
import scss from 'highlight.js/lib/languages/scss';
import sql from 'highlight.js/lib/languages/sql';
import swift from 'highlight.js/lib/languages/swift';
import typescript from 'highlight.js/lib/languages/typescript';
import xml from 'highlight.js/lib/languages/xml';
import yaml from 'highlight.js/lib/languages/yaml';
import ini from 'highlight.js/lib/languages/ini';
import dockerfile from 'highlight.js/lib/languages/dockerfile';

const LANGUAGES = {
  bash, c, cpp, csharp, css, go, java, javascript, json, kotlin, markdown, php, powershell, python, ruby, rust,
  scss, sql, swift, typescript, xml, yaml, ini, dockerfile,
};
for (const [name, language] of Object.entries(LANGUAGES)) hljs.registerLanguage(name, language);

const BY_EXTENSION: Record<string, string> = {
  sh: 'bash', bash: 'bash', zsh: 'bash',
  c: 'c', h: 'c',
  cc: 'cpp', cpp: 'cpp', cxx: 'cpp', hpp: 'cpp', hh: 'cpp',
  cs: 'csharp', csx: 'csharp',
  css: 'css', scss: 'scss', less: 'scss',
  go: 'go',
  java: 'java',
  js: 'javascript', jsx: 'javascript', mjs: 'javascript', cjs: 'javascript',
  ts: 'typescript', tsx: 'typescript', mts: 'typescript', cts: 'typescript',
  // Vue and Svelte single-file components: the markup reads well enough as HTML.
  vue: 'xml', svelte: 'xml', html: 'xml', htm: 'xml', xml: 'xml', svg: 'xml', csproj: 'xml', props: 'xml',
  json: 'json', jsonc: 'json',
  kt: 'kotlin', kts: 'kotlin',
  md: 'markdown', markdown: 'markdown',
  php: 'php',
  ps1: 'powershell', psm1: 'powershell', psd1: 'powershell',
  py: 'python', pyi: 'python',
  rb: 'ruby',
  rs: 'rust',
  sql: 'sql',
  swift: 'swift',
  yml: 'yaml', yaml: 'yaml',
  toml: 'ini', ini: 'ini', cfg: 'ini',
};

/** The highlight.js language for a file path, if one is registered for it. */
export function languageFor(path: string): string | null {
  const name = path.split('/').pop()?.toLowerCase() ?? '';
  if (name === 'dockerfile' || name.endsWith('.dockerfile')) return 'dockerfile';
  const extension = name.includes('.') ? name.split('.').pop()! : '';
  return BY_EXTENSION[extension] ?? null;
}

function escape(text: string): string {
  return text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
}

/** Cut highlighted HTML into lines, closing the spans open at each line end and reopening them. */
export function splitHighlighted(html: string): string[] {
  const lines: string[] = [];
  const open: string[] = [];
  let line = '';
  for (const part of html.split(/(<span[^>]*>|<\/span>|\n)/)) {
    if (!part) continue;
    if (part === '\n') {
      lines.push(line + '</span>'.repeat(open.length));
      line = open.join('');
    } else if (part.startsWith('<span')) {
      open.push(part);
      line += part;
    } else if (part === '</span>') {
      open.pop();
      line += part;
    } else {
      line += part;
    }
  }
  lines.push(line + '</span>'.repeat(open.length));
  return lines;
}

/** One HTML string per line: highlighted when the language is known, else just escaped. */
export function highlightLines(lines: string[], path: string): string[] {
  const language = languageFor(path);
  if (!language || !lines.length) return lines.map(escape);
  const html = hljs.highlight(lines.join('\n'), { language, ignoreIllegals: true }).value;
  const out = splitHighlighted(html);
  // highlight.js never adds or drops a line break, but never show the wrong code if it did.
  return out.length === lines.length ? out : lines.map(escape);
}
