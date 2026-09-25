import type { AnalysisClaim, AnalysisEvidence, ArtifactRecord } from './client';

export type MeetingTopic = {text: string; count: number; claimIds: string[]};

// Migration support for existing meetings without model-authored topic metadata.
// These are domain recognizers, never a bag of words or the model's allowed list.
const domains: [string, RegExp][] = [
  ['教育应用', /教育|教学|课堂|学校|学生|老师|教师|课程|学习兴趣|学习动力|\b(education|educational|schools?|students?|teachers?|classrooms?|curriculum|pedagogy)\b/iu],
  ['政府', /政府|政务|公共政策|公共服务|公共治理|监管|立法|\b(governments?|regulation|regulators?|legislation|public policy|public services?)\b/iu],
  ['医疗', /医疗|医学|医院|患者|诊疗|疾病|\b(healthcare|medical|medicine|hospitals?|patients?)\b/iu],
  ['经济', /经济|就业|劳动力|产业发展|\b(economy|economic|employment|labor market)\b/iu],
  ['商业', /商业|商业模式|市场竞争|市场需求|盈利|营收|\b(business models?|revenue|profitability|market competition)\b/iu],
  ['金融', /金融|融资|投资|银行|资本市场|\b(finance|financial|investment|banking|funding)\b/iu],
  ['用户体验', /用户体验|交互|易用性|可用性|触屏|触摸屏|字幕|\b(user experience|usability|touchscreens?|subtitles?|captions?)\b/iu],
  ['隐私安全', /隐私|信息安全|网络安全|数据安全|\b(privacy|cybersecurity|data security)\b/iu],
  ['环境', /环境保护|气候|碳排放|可持续发展|\b(climate|emissions|sustainability|environmental)\b/iu],
  ['能源', /能源|电力|电网|光伏|\b(energy|electricity|power grid|solar power)\b/iu],
  ['文化', /文化|艺术|文学|\b(culture|cultural|arts|literature)\b/iu],
  ['科研', /科研|科学研究|基础研究|\b(scientific research|science|academia)\b/iu],
];
const aliases: Record<string, string> = {
  '教育教学':'教育应用', '学校教育':'教育应用', '教育科技':'教育应用', '教育技术':'教育应用',

  '人工智能技术':'人工智能',
  '交互设计':'用户体验', '人机交互':'用户体验', '产品体验':'用户体验',
  '数据隐私':'隐私安全', '数据安全':'隐私安全', '隐私保护':'隐私安全',
};
const generic = new Set('AI ai Data data speaker participants gratitude one needed how process use change traditional generative 人工智能 数据 科技 技术 流程 过程 方法 如何 如何使用 生成式人工智能 用户 使用 过滤 存在 设备 需要 发言 说话人 参与者 感谢 问题 讨论 会议 内容 情况 方面 主题 需求 资源 设置 数量 不确定性 学习兴趣'.split(' '));
function topicLabel(label: string): string | null {
  const value = aliases[label.trim()] ?? label.trim();
  // A topic must name something concrete; keep complete noun phrases intact.
  if (generic.has(value.toLowerCase()) || /^(ai|data|how to|process|processes|technology|generative ai)$/i.test(value)) return null;
  if (/^(how to|how do|how can|如何|怎么)/iu.test(value)) return null;
  if (!/^[\p{L}\p{N}][\p{L}\p{N} &/·–-]{1,47}$/u.test(value)) return null;
  if (/^[\p{Script=Han}]+$/u.test(value) ? value.length > 12 : value.split(/\s+/).length > 5) return null;
  return value;
}
function sourceKey(e: AnalysisEvidence): string {
  return e.kind === 'source' ? `${e.session_id}:${e.span.segment_id}:${e.span.segment_revision}`
    : `${e.artifact_id}:${e.revision}`;
}
function claimDomains(claim: AnalysisClaim): string[] {
  const text = [claim.text, ...claim.evidence.map(e => e.kind === 'source' ? e.span.quote : e.quote)].join('\n');
  return domains.filter(([, pattern]) => pattern.test(text)).map(([label]) => label);
}

/** Compose the whole meeting's source-backed topic map, not just its latest note.
 * Source identity prevents rolling overlap/repeated model outputs inflating rank.
 * Caller supplies only current, visible artifacts belonging to this session.
 */
export function topicsFromHistory(artifacts: ArtifactRecord[], claims: AnalysisClaim[]): MeetingTopic[] {
  const topics = new Map<string, {sources: Set<string>; claimIds: Set<string>}>();
  const supported = claims.filter(c => c.grounding === 'cited' && c.evidence.length);
  const claimSources = supported.map(c => ({claim:c, sources:new Set(c.evidence.map(sourceKey)), domains:claimDomains(c)}));
  const add = (label: string, evidence: AnalysisEvidence[]) => {
    const sources = evidence.map(sourceKey);
    if (!sources.length) return;
    const topic = topics.get(label) ?? {sources:new Set<string>(), claimIds:new Set<string>()};
    sources.forEach(key => topic.sources.add(key));
    for (const entry of claimSources) {
      if (entry.domains.includes(label) || sources.some(key => entry.sources.has(key))) topic.claimIds.add(entry.claim.claim_id);
    }
    topics.set(label, topic);
  };
  // Chronological insertion makes equally supported earlier themes stable.
  for (const artifact of [...artifacts].sort((a,b) => a.created_at_ms-b.created_at_ms || a.artifact_id.localeCompare(b.artifact_id))) {
    for (const section of artifact.content.sections) {
      if (section.topics?.length) {
        for (const topic of section.topics) {
          const label = topicLabel(topic.label);
          if (label) add(label, topic.evidence);
        }
      } else {
        for (const claim of section.claims) {
          if (claim.grounding !== 'cited' || !claim.evidence.length) continue;
          for (const label of claimDomains(claim)) add(label, claim.evidence);
        }
      }
    }
  }
  return [...topics].map(([text, topic]) => ({text,count:topic.sources.size,claimIds:[...topic.claimIds]}))
    .sort((a,b) => b.count-a.count).slice(0,5);
}
