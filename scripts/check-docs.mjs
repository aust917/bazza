import { execFileSync } from "node:child_process";
import { existsSync, readFileSync, realpathSync } from "node:fs";
import { dirname, isAbsolute, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../", import.meta.url));
const files = [...new Set(execFileSync("git", [
  "ls-files", "--cached", "--others", "--exclude-standard", "-z",
], { cwd: root, encoding: "utf8" }).split("\0").filter(Boolean))];
const docs = files.filter((file) => file.endsWith(".md")
  && !file.startsWith("docs/archive/") && existsSync(resolve(root, file)));
const errors = [];

function insideRepo(path) {
  const pathFromRoot = relative(root, path);
  return !isAbsolute(pathFromRoot) && pathFromRoot !== ".."
    && !pathFromRoot.startsWith(`..${sep}`);
}

// Preserve line numbers while removing fenced examples. This intentionally
// covers ordinary Markdown links, not every extension of the Markdown grammar.
function withoutFences(source) {
  let fence;
  return source.split("\n").map((line) => {
    const marker = line.match(/^ {0,3}(`{3,}|~{3,})(.*)$/);
    if (fence) {
      if (marker && marker[1][0] === fence[0]
        && marker[1].length >= fence.length && !marker[2].trim()) fence = undefined;
      return "";
    }
    if (marker) {
      fence = marker[1];
      return "";
    }
    return line;
  }).join("\n");
}

for (const file of docs) {
  const source = readFileSync(resolve(root, file), "utf8");
  const budget = file === "AGENTS.md" ? 4096
    : file.endsWith("/AGENTS.md") ? 3072
      : file === ".github/copilot-instructions.md" ? 1024 : undefined;
  const bytes = Buffer.byteLength(source, "utf8");
  if (budget && bytes > budget) errors.push(`${file}: ${bytes} bytes exceeds ${budget}-byte instruction budget`);

  const markdown = withoutFences(source);
  const patterns = [
    /!?\[[^\]\n]*\]\(\s*(?:<([^>\n]+)>|([^\s)]+))/g,
    /^ {0,3}\[[^\]\n]+\]:[ \t]*(?:<([^>\n]+)>|(\S+))/gm,
  ];
  for (const pattern of patterns) {
    for (const match of markdown.matchAll(pattern)) {
      const target = match[1] ?? match[2];
      if (/^(?:[a-z][a-z0-9+.-]*:|\/\/|#)/i.test(target)) continue;
      const pathname = target.split(/[?#]/, 1)[0];
      if (!pathname) continue;
      const line = markdown.slice(0, match.index).split("\n").length;
      let destination;
      try {
        destination = resolve(dirname(resolve(root, file)), decodeURIComponent(pathname));
      } catch {
        errors.push(`${file}:${line}: invalid link target ${target}`);
        continue;
      }
      if (!insideRepo(destination)
        || (existsSync(destination) && !insideRepo(realpathSync(destination)))) {
        errors.push(`${file}:${line}: link leaves repository: ${target}`);
      } else if (!existsSync(destination)) {
        errors.push(`${file}:${line}: missing link target: ${target}`);
      }
    }
  }
}

if (errors.length) {
  console.error(errors.join("\n"));
  process.exitCode = 1;
} else {
  console.log(`docs: ${docs.length} Markdown files checked; instruction budgets OK.`);
}
