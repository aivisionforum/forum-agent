import type { ArtifactPublishCommand, ForumClient } from './client';

/** The visible anonymous proposal is the approval. Opening a window is a
 * separate outcome: a window failure must never look like a failed publication. */
export async function publishReviewedInsight(
  client: ForumClient, proposal: ArtifactPublishCommand, sessionId: string, language: 'zh' | 'en'
) {
  const artifact = await client.approveAndPublishArtifact(proposal);
  let windowError = '';
  try { await client.showInsightWall(sessionId, language); }
  catch (error) { windowError = String(error); }
  return { artifact, windowError };
}
