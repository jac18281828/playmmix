import * as fs from 'fs';
import * as path from 'path';

// The tag workflow cannot run before merge, so this spec reads it as text and
// pins the parts a stack deploy depends on.
const repoRoot = path.join(__dirname, '..');
const workflow = fs.readFileSync(
  path.join(repoRoot, '.github', 'workflows', 'deploy-static-site.yml'),
  'utf8',
);

// cdk/www.ts is read, not imported: importing it constructs an App.
const stackId = (() => {
  const app = fs.readFileSync(path.join(__dirname, 'www.ts'), 'utf8');
  const match = /new PlaymmixStack\(app, '([^']+)'/.exec(app);
  if (!match) {
    throw new Error('cdk/www.ts no longer constructs a PlaymmixStack with a literal id');
  }
  return match[1];
})();

// One entry per job step: the text from its `- ` line up to the next step's.
const steps = workflow.split(/\n(?= {6}- )/).slice(1);

function stepIndex(describes: (step: string) => boolean, label: string): number {
  const index = steps.findIndex(describes);
  if (index < 0) {
    throw new Error(`deploy-static-site.yml has no ${label} step`);
  }
  return index;
}

const named = (name: string) => (step: string) => new RegExp(`- name: ${name}\\n`).test(step);

describe('deploy-static-site.yml', () => {
  it('deploys the stack cdk/www.ts names, without an approval prompt', () => {
    const deploy = steps[stepIndex(named('Deploy infrastructure'), '"Deploy infrastructure"')];
    expect(deploy).toContain(
      `run: bun run cdk:deploy -- ${stackId} --require-approval never --no-telemetry`,
    );
  });

  it('deploys after the build and before the bundle lands in the bucket', () => {
    const order = [
      stepIndex(named('Configure AWS Credentials'), '"Configure AWS Credentials"'),
      stepIndex((step) => step.includes('uses: oven-sh/setup-bun'), 'oven-sh/setup-bun'),
      stepIndex((step) => step.includes('bun install --frozen-lockfile'), 'bun install'),
      stepIndex(named('Build Artifact'), '"Build Artifact"'),
      stepIndex(named('Deploy infrastructure'), '"Deploy infrastructure"'),
      stepIndex(named('Sync S3 Bucket'), '"Sync S3 Bucket"'),
    ];
    expect(order).toEqual([...order].sort((a, b) => a - b));
  });

  it('installs trunk with --locked', () => {
    const installs = workflow.split('\n').filter((line) => line.includes('cargo install trunk'));
    expect(installs.length).toBeGreaterThan(0);
    for (const line of installs) {
      expect(line).toContain('--locked');
    }
  });
});
