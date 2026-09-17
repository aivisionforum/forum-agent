// Run the existing TypeScript node:test suite on the project's Node 20 baseline.
// Preserve source files for the inherited source-inspection tests, and emit only
// into a uniquely owned temporary directory that is removed after every run.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import ts from 'typescript';

const uiRoot = fileURLToPath(new URL('..', import.meta.url));
const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'forum-ui-tests-'));

function emitTypeScript(directory) {
  for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
    const sourcePath = path.join(directory, entry.name);
    if (entry.isDirectory()) {
      emitTypeScript(sourcePath);
    } else if (entry.name.endsWith('.ts') && !entry.name.endsWith('.d.ts')) {
      const source = fs.readFileSync(sourcePath, 'utf8');
      const { outputText } = ts.transpileModule(source, {
        fileName: sourcePath,
        compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext }
      });
      const target = path.join(temporary, path.relative(uiRoot, sourcePath).replace(/\.ts$/, '.mjs'));
      fs.mkdirSync(path.dirname(target), { recursive: true });
      fs.writeFileSync(target, outputText.replace(/(from\s+['"][^'"]+)\.ts(['"])/g, '$1.mjs$2'));
    }
  }
}

try {
  for (const directory of ['src', 'tests']) {
    fs.cpSync(path.join(uiRoot, directory), path.join(temporary, directory), { recursive: true });
    emitTypeScript(path.join(uiRoot, directory));
  }
  const testFiles = fs.readdirSync(path.join(uiRoot, 'tests'))
    .filter(name => name.endsWith('.test.ts'))
    .map(name => path.join(temporary, 'tests', name.replace(/\.ts$/, '.mjs')));
  if (testFiles.length === 0) throw new Error('No TypeScript test files were found');
  const result = spawnSync(process.execPath, ['--test', ...testFiles], { stdio: 'inherit' });
  if (result.error) throw result.error;
  process.exitCode = result.status ?? 1;
} finally {
  fs.rmSync(temporary, { recursive: true, force: true });
}
