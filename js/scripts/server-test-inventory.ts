import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";
import { checkUpstream, upstream, UPSTREAM_SHA } from "../tests/server-parity/upstream.ts";

checkUpstream();
const ts = createRequire(resolve(upstream, "package.json"))("typescript");
const tests: { source: string; name: string; rustName: string }[] = [];

/**
 * The literal an `it.each` case contributes, as the title interpolation sees it:
 * a string for `%s` and `%j`, and the textual form of every other literal.
 */
function unwrap(node: any): any {
  // `it.each([...] as const)` and `satisfies` wrap the array literal
  while (node && (ts.isAsExpression(node) || ts.isSatisfiesExpression(node))) node = node.expression;
  return node;
}

function interpolate(template: string, value: any): string {
  if (!ts.isStringLiteral(value)) {
    if (ts.isNumericLiteral(value) || ts.isTrueKeyword(value) || ts.isFalseKeyword(value)) {
      return template.replace(/%[sj]/g, value.getText());
    }
    throw new Error(`Unsupported it.each case ${value.getText()}`);
  }
  return template.replace(/%[sj]/g, value.text);
}

for (const component of ["core", "acl", "proxy", "static"]) {
  const source = `packages/server/${component}/tests/index.spec.ts`;
  const code = readFileSync(resolve(upstream, source), "utf8");
  const file = ts.createSourceFile(source, code, ts.ScriptTarget.Latest, true);
  function visit(node: any) {
    if (ts.isCallExpression(node)) {
      const title = node.arguments[0];
      const call = node.expression;
      let names: string[] = [];
      if (ts.isIdentifier(call) && call.text === "it" && title && ts.isStringLiteral(title)) {
        names = [title.text];
      } else if (ts.isCallExpression(call) && ts.isPropertyAccessExpression(call.expression)
        && call.expression.expression.getText(file) === "it" && call.expression.name.text === "each") {
        const cases = unwrap(call.arguments[0]);
        if (!title || !ts.isStringLiteral(title) || !cases || !ts.isArrayLiteralExpression(cases)) {
          throw new Error(`Unsupported it.each declaration in ${source}`);
        }
        names = cases.elements.map((value: any) => {
          // A case may be an array of arguments; the title interpolates its first string literal.
          const row = unwrap(value);
          const first = ts.isArrayLiteralExpression(row) ? row.elements[0] : row;
          if (!first) throw new Error(`Empty it.each case in ${source}`);
          return interpolate(title.text, first);
        });
      }
      for (const name of names) {
        tests.push({ source, name, rustName: name.replace(/[^a-zA-Z0-9]+/g, "_").replace(/^_|_$/g, "").toLowerCase() });
      }
    }
    ts.forEachChild(node, visit);
  }
  visit(file);
}
const output = JSON.stringify({ upstream: UPSTREAM_SHA, tests }, null, 2) + "\n";
const path = fileURLToPath(new URL("../tests/server-parity/inventory.json", import.meta.url));
if (process.argv.includes("--check")) {
  if (readFileSync(path, "utf8").replace(/\r\n/g, "\n") !== output) throw new Error("Server test inventory is stale; run bun run server:inventory");
} else {
  writeFileSync(path, output);
}
console.log(`${tests.length} upstream test cases inventoried (parameterized declarations expanded)`);
