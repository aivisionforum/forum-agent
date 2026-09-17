export const MAX_LEGACY_IMPORT_BYTES = 16 * 1024 * 1024;
export async function readLegacyFile(file: { name: string; size: number; text(): Promise<string> }): Promise<{ title: string; content: string }> {
  if (file.size > MAX_LEGACY_IMPORT_BYTES) throw new Error('旧记录文件不能超过 16 MiB。');
  if (!file.size) throw new Error('请选择非空的 JSONL 记录。');
  const content = await file.text();
  if (new TextEncoder().encode(content).length > MAX_LEGACY_IMPORT_BYTES) throw new Error('旧记录内容不能超过 16 MiB。');
  if (!content.trim()) throw new Error('旧记录没有可导入的内容。');
  return { title: file.name.replace(/\.[^.]+$/, '').replace(/[\u0000-\u001f]/g, '').slice(0,180) || '导入的会议记录', content };
}
