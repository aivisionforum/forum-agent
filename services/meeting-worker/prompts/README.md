# 版本化提示

实际随包资源位于 `../src/forum_meeting_worker/prompts/v1/{kind}.txt`。
每个文件包含完整提示与输出契约；宿主直接计算文件原始 UTF-8 字节的 SHA256。
worker 的 `prompts.load_prompt` 不拼接、strip 或替换内容。
任何提示修改都应改变 prompt_version 并重新核对配置/检查点兼容性。
