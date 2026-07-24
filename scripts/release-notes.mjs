import { appendFileSync, readFileSync } from 'node:fs';

function fail(message) {
  process.stderr.write(`发布日志校验失败: ${message}\n`);
  process.exit(1);
}

const cargoToml = readFileSync(new URL('../src-tauri/Cargo.toml', import.meta.url), 'utf8');
const packageSection = cargoToml
  .split(/(?=^\[[^\]]+\]\s*$)/m)
  .find((section) => /^\[package\]\s*$/m.test(section));
const version = packageSection?.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
if (!version) fail('无法从 src-tauri/Cargo.toml 读取 package version');

const changelog = readFileSync(new URL('../CHANGELOG.md', import.meta.url), 'utf8');
const heading = `## [${version}]`;
const escapedVersion = version.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
const headingMatch = changelog.match(
  new RegExp(`^## \\[${escapedVersion}\\] - \\d{4}-\\d{2}-\\d{2}\\s*$`, 'm'),
);
if (headingMatch?.index === undefined) {
  fail(`CHANGELOG.md 缺少“${heading} - YYYY-MM-DD”章节`);
}
const start = headingMatch.index;

const afterHeading = changelog.indexOf('\n', start);
const nextVersion = changelog.indexOf('\n## [', afterHeading);
const section = changelog.slice(start, nextVersion < 0 ? changelog.length : nextVersion).trim();
const notes = section.slice(section.indexOf('\n') + 1).trim();
const bullets = notes.match(/^-\s+\S.+$/gm) ?? [];
if (bullets.length === 0) fail(`${heading} 必须逐条列出至少一项更新内容`);
if (/\b(?:TODO|TBD)\b|待补充|暂无更新/i.test(notes)) {
  fail(`${heading} 仍包含占位内容`);
}

const tag = process.env.GITHUB_REF_NAME;
if (tag?.startsWith('v') && tag.slice(1) !== version) {
  fail(`tag ${tag} 与 Cargo 版本 ${version} 不一致`);
}

if (process.argv.includes('--github-output')) {
  const output = process.env.GITHUB_OUTPUT;
  if (!output) fail('缺少 GitHub Actions 的 GITHUB_OUTPUT 环境变量');
  const delimiter = `LOGCRATE_RELEASE_NOTES_${Date.now()}`;
  const body = `${notes}\n\n---\n安装包见下方 Assets。`;
  appendFileSync(output, `body<<${delimiter}\n${body}\n${delimiter}\n`);
} else {
  process.stdout.write(`发布日志校验通过: v${version}，共 ${bullets.length} 项更新。\n`);
}
