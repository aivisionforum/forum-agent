import test from 'node:test';
import assert from 'node:assert/strict';
import { publishReviewedInsight } from '../src/lib/forum/publish-insight.ts';
import { ForumClient, canReviewForWall, type ArtifactPublishCommand } from '../src/lib/forum/client.ts';
import { DesktopTransport } from '../src/lib/forum/transport.ts';
import { previewAnalysisState } from '../src/lib/forum/preview.ts';

const proposal = {artifact_id:'artifact',expected_revision:4,reviewed_text:'发言人A：已核对的公开正文。'} as ArtifactPublishCommand;
test('one click approves the exact preview before opening the session wall', async () => {
  const calls: string[] = [];
  const client = new ForumClient(new DesktopTransport(async (name, args) => {
    calls.push(name);
    if (name === 'approve_and_publish_artifact') {
      assert.equal(args?.request, proposal);
      return {...previewAnalysisState.artifacts[0],publication:'published'} as never;
    }
    assert.deepEqual(args, {sessionId:'this-session',language:'en'});
    return {} as never;
  }));
  const result = await publishReviewedInsight(client, proposal, 'this-session', 'en');
  assert.deepEqual(calls, ['approve_and_publish_artifact','show_insight_wall']);
  assert.equal(result.artifact.publication, 'published');
  assert.equal(result.windowError, '');
});
test('failed approval does not open an empty wall or silently retry with a new preview', async () => {
  const calls: string[] = [];
  const client = new ForumClient(new DesktopTransport(async name => {calls.push(name); throw new Error('revision conflict');}));
  await assert.rejects(publishReviewedInsight(client, proposal, 'session', 'zh'), /revision conflict/);
  assert.deepEqual(calls, ['approve_and_publish_artifact']);
});
test('window failure preserves successful publication and lets the UI offer only a window retry', async () => {
  const client = new ForumClient(new DesktopTransport(async name => {
    if (name === 'show_insight_wall') throw new Error('window unavailable');
    return {...previewAnalysisState.artifacts[0],publication:'published'} as never;
  }));
  const result = await publishReviewedInsight(client, proposal, 'session', 'zh');
  assert.equal(result.artifact.publication, 'published');
  assert.match(result.windowError, /window unavailable/);
});
test('partial live points require citations, while whole-session documents require full coverage', () => {
  const a = structuredClone(previewAnalysisState.artifacts[0]);
  a.coverage_complete = false;
  assert.equal(canReviewForWall(a), true);
  for (const kind of ['minutes','closing_brief','event_report'] as const) assert.equal(canReviewForWall({...a,kind}), false);
  a.content.sections[0].claims[0].evidence = [];
  assert.equal(canReviewForWall(a), false);
  a.content.sections = [];
  assert.equal(canReviewForWall(a), false);
});
