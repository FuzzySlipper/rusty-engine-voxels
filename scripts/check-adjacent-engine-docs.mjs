import { readdir, readFile } from 'node:fs/promises';
import { join, relative } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

export function validateAdjacentEngineDocs(documents) {
  const removedCarrier =
    /\bengine-source(?:\.json)?\b|\bengine-development(?:\.json)?\b|\bscripts\/engine-revision\b|\bscripts\/studio\.sh\b/iu;

  for (const [name, content] of Object.entries(documents)) {
    const statements = content
      .split(/\n\s*\n/u)
      .flatMap((paragraph) =>
        paragraph.replace(/\n+/gu, ' ').split(/(?<=[.!?;])(?:\s+|$)/u),
      )
      .map((statement) => statement.trim())
      .filter(Boolean)
      .flatMap((statement) => {
        const clauses = statement.split(
          /,\s*(?=(?:and|although|but|despite|though|while|whereas|yet)\b)/iu,
        );
        return clauses.flatMap((clause) => {
          const introductoryAbsence = clause.match(
            /^\s*(without\b[^,]*),\s*(.+)$/iu,
          );
          return introductoryAbsence
            ? [introductoryAbsence[1], introductoryAbsence[2]]
            : [clause];
        });
      })
      .map((clause) => clause.trim())
      .filter(Boolean);

    for (const statement of statements) {
      const mentionsEngine = /\bEngine\b/iu.test(statement);
      const namesEnginePin =
        /\bEngine(?:'s| provider)? (?:is |remains |was |has been )?pin(?:ned|ning|s)?\b|\bpin(?:ned|ning|s)? (?:the )?Engine\b|\bEngine(?:'s| provider)?\b[^.!?;]*\bnot unpinned\b/iu.test(
          statement,
        );
      const stale =
        /\bprovider pin\b/iu.test(statement) ||
        (mentionsEngine && namesEnginePin) ||
        /\bexact public Rusty Engine\b/iu.test(statement) ||
        removedCarrier.test(statement);
      const explicitlyHistorical =
        /^(?:historical(?:ly)?|older|previous|retired)\b/iu.test(statement) &&
        !/\b(?:current|live|now|present|production|today)\b/iu.test(statement);
      const explicitlyAbsent =
        /^(?:(?:this|the) (?:project|repository) )?(?:has|have) no (?:provider|Engine) pin[.!?;]?$|^there is no (?:provider|Engine) pin[.!?;]?$|^without (?:an? )?(?:provider|Engine) pin[.!?;]?$/iu.test(
          statement,
        );

      if (stale && !explicitlyHistorical && !explicitlyAbsent) {
        throw new Error(
          `${name} contains stale current Engine dependency guidance: ${statement}`,
        );
      }
    }
  }
}

async function markdownFiles(directory) {
  const entries = await readdir(directory, { withFileTypes: true });
  const files = [];
  for (const entry of entries) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) {
      files.push(...(await markdownFiles(path)));
    } else if (entry.isFile() && entry.name.endsWith('.md')) {
      files.push(path);
    }
  }
  return files;
}

async function loadCurrentDocs(root) {
  const paths = [join(root, 'AGENTS.md'), join(root, 'README.md')];
  for (const path of await markdownFiles(join(root, 'docs'))) {
    if (relative(root, path) !== 'docs/session.md') {
      paths.push(path);
    }
  }
  return Object.fromEntries(
    await Promise.all(
      paths.map(async (path) => [relative(root, path), await readFile(path, 'utf8')]),
    ),
  );
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const root = fileURLToPath(new URL('..', import.meta.url));
  validateAdjacentEngineDocs(await loadCurrentDocs(root));
  console.log('Adjacent Engine documentation policy passed.');
}
