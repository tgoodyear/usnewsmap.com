import { createHash } from "node:crypto";
import { existsSync, readFileSync, statSync } from "node:fs";
import { dirname, relative, resolve } from "node:path";
import ts from "typescript";

/** Hash source content, not emitted chunks, timestamps, or asset filenames. */
export function pageFingerprint(root: string, entry: string): string {
  const sources = new Map<string, string>();
  const visit = (file: string) => {
    if (sources.has(file)) return;
    const source = readFileSync(file, "utf8");
    sources.set(file, source.replace(/<!--[\s\S]*?-->/g, ""));
    if (!/\.[jt]sx?$/.test(file)) return;
    const tree = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true);
    sources.set(file, ts.createPrinter({ removeComments: true }).printFile(tree));
    const walk = (node: ts.Node) => {
      let specifier: string | undefined;
      if (ts.isImportDeclaration(node) && !node.importClause?.isTypeOnly && ts.isStringLiteral(node.moduleSpecifier)) {
        specifier = node.moduleSpecifier.text;
      } else if (
        ts.isExportDeclaration(node) &&
        !node.isTypeOnly &&
        node.moduleSpecifier &&
        ts.isStringLiteral(node.moduleSpecifier)
      ) {
        specifier = node.moduleSpecifier.text;
      } else if (
        ts.isCallExpression(node) &&
        node.expression.kind === ts.SyntaxKind.ImportKeyword &&
        node.arguments[0] &&
        ts.isStringLiteral(node.arguments[0])
      ) {
        specifier = node.arguments[0].text;
      }
      if (specifier?.startsWith(".")) {
        const path = resolve(dirname(file), specifier.split("?")[0]!);
        const dependency = [
          path,
          `${path}.tsx`,
          `${path}.ts`,
          `${path}.jsx`,
          `${path}.js`,
          `${path}/index.tsx`,
          `${path}/index.ts`,
        ].find((candidate) => existsSync(candidate) && statSync(candidate).isFile());
        if (!dependency) throw new Error(`Missing sitemap content source: ${path}`);
        visit(dependency);
      }
      ts.forEachChild(node, walk);
    };
    walk(tree);
  };
  visit(resolve(root, entry));
  visit(resolve(root, "index.html"));
  const hash = createHash("sha256");
  // Relative names make Docker (/web) and local builds equivalent.
  for (const [file, text] of [...sources].sort(([a], [b]) => a.localeCompare(b))) {
    hash.update(relative(root, file)).update("\0").update(text).update("\0");
  }
  return hash.digest("hex");
}
